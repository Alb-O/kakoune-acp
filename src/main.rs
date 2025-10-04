use std::{path::PathBuf, sync::Arc};

use agent_client_protocol as acp;
use agent_client_protocol::{
    Agent, ClientCapabilities, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    SessionId, StopReason, V1,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::Parser;
use futures::io::{AsyncRead, AsyncWrite};
use log::{error, info};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Parser, Debug)]
#[command(author, version, about = "Interact with ACP agents from Kakoune.")]
struct Args {
    /// Path to the agent executable to spawn.
    #[arg(long, env = "KAKOUNE_ACP_AGENT")]
    agent: String,

    /// Additional arguments to pass to the agent executable.
    #[arg(long = "agent-arg", value_name = "ARG")]
    agent_args: Vec<String>,

    /// Working directory to launch the agent in.
    #[arg(long)]
    agent_workdir: Option<PathBuf>,

    /// Kakoune session identifier (defaults to $kak_session).
    #[arg(long, env = "kak_session")]
    session: Option<String>,

    /// Kakoune client to target (defaults to $kak_client).
    #[arg(long, env = "kak_client")]
    client: Option<String>,

    /// Prompt to send to the agent. When omitted it is read from stdin.
    #[arg(long)]
    prompt: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();

    let args = Args::parse();

    let session = args
        .session
        .or_else(|| std::env::var("kak_session").ok())
        .ok_or_else(|| anyhow!("Kakoune session not provided (pass --session or set kak_session)"))?;

    let client = args
        .client
        .or_else(|| std::env::var("kak_client").ok())
        .ok_or_else(|| anyhow!("Kakoune client not provided (pass --client or set kak_client)"))?;

    let prompt = match args.prompt {
        Some(p) => p,
        None => read_prompt_from_stdin().await?,
    };

    if prompt.trim().is_empty() {
        return Err(anyhow!("Prompt is empty"));
    }

    let dispatcher = Arc::new(KakouneDispatcher::new(session, client));
    dispatcher
        .show_status("Connecting to agent…")
        .await
        .context("failed to notify Kakoune about ACP status")?;

    let (outgoing, incoming, mut child) = spawn_agent(&args.agent, &args.agent_args, args.agent_workdir.as_deref())
        .context("failed to start agent process")?;

    let local_set = LocalSet::new();
    let dispatcher_for_run = dispatcher.clone();
    let acp_result: Result<(SessionId, StopReason)> = local_set
        .run_until(async move {
            let kakoune_client = KakouneAcpClient::new(dispatcher_for_run.clone());
            let (connection, io_handler) =
                acp::ClientSideConnection::new(kakoune_client, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });
            tokio::task::spawn_local(io_handler);

            let init_response = connection
                .initialize(InitializeRequest {
                    protocol_version: V1,
                    client_capabilities: ClientCapabilities::default(),
                    meta: None,
                })
                .await
                .context("initialize call failed")?;
            info!("Connected to agent supporting {:?}", init_response.agent_capabilities);

            let new_session = connection
                .new_session(NewSessionRequest {
                    mcp_servers: Vec::new(),
                    cwd: std::env::current_dir().context("unable to determine current directory")?,
                    meta: None,
                })
                .await
                .context("failed to create ACP session")?;

            dispatcher_for_run
                .begin_conversation(&new_session.session_id)
                .await
                .context("failed to announce ACP session in Kakoune")?;

            connection
                .prompt(PromptRequest {
                    session_id: new_session.session_id.clone(),
                    prompt: vec![ContentBlock::from(prompt.clone())],
                    meta: None,
                })
                .await
                .map(|response| (new_session.session_id.clone(), response.stop_reason))
                .context("prompt request failed")
        })
        .await;

    // Always ensure the child process is terminated.
    if let Err(e) = child.start_kill() {
        error!("failed to signal agent process for termination: {e}");
    }
    let _ = child.wait().await;

    match acp_result {
        Ok((session_id, stop_reason)) => {
            dispatcher
                .finish_conversation(&session_id, &stop_reason)
                .await
                .context("failed to update Kakoune with ACP result")?;
            Ok(())
        }
        Err(err) => {
            dispatcher
                .report_error(&format!("ACP error: {err:#}"))
                .await
                .ok();
            Err(err)
        }
    }
}

async fn read_prompt_from_stdin() -> Result<String> {
    let mut buffer = String::new();
    let mut stdin = io::stdin();
    stdin
        .read_to_string(&mut buffer)
        .await
        .context("failed to read prompt from stdin")?;
    Ok(buffer)
}

fn spawn_agent(
    program: &str,
    args: &[String],
    workdir: Option<&std::path::Path>,
) -> Result<(
    impl AsyncWrite + Unpin,
    impl AsyncRead + Unpin,
    tokio::process::Child,
)> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);

    let mut child = command.spawn().context("unable to spawn agent process")?;
    let outgoing = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("agent stdin unavailable"))?
        .compat_write();
    let incoming = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("agent stdout unavailable"))?
        .compat();

    Ok((outgoing, incoming, child))
}

struct KakouneDispatcher {
    session: String,
    client: String,
    accumulated_output: Mutex<String>,
}

impl KakouneDispatcher {
    fn new(session: String, client: String) -> Self {
        Self {
            session,
            client,
            accumulated_output: Mutex::new(String::new()),
        }
    }

    async fn begin_conversation(&self, session_id: &SessionId) -> Result<()> {
        {
            let mut buffer = self.accumulated_output.lock().await;
            buffer.clear();
            buffer.push_str(&format!("Session {}\n", session_id.0));
        }
        self.show_status("Awaiting agent response…").await
    }

    async fn append_agent_message(&self, chunk: &str) -> Result<()> {
        let display = {
            let mut buffer = self.accumulated_output.lock().await;
            buffer.push_str(chunk);
            buffer.clone()
        };
        self.show_info(&display).await
    }

    async fn finish_conversation(&self, _session_id: &SessionId, reason: &StopReason) -> Result<()> {
        let summary = match reason {
            StopReason::EndTurn => "Agent turn complete.".to_string(),
            StopReason::MaxTokens => {
                "Agent stopped after reaching the configured token limit.".to_string()
            }
            StopReason::MaxTurnRequests => {
                "Agent stopped after reaching the maximum number of tool calls.".to_string()
            }
            StopReason::Refusal => "Agent refused to continue.".to_string(),
            StopReason::Cancelled => "Agent run cancelled.".to_string(),
        };
        self.show_status(&summary).await
    }

    async fn report_error(&self, message: &str) -> Result<()> {
        self.show_info(message).await
    }

    async fn show_status(&self, message: &str) -> Result<()> {
        self.send_to_kak(&format!(
            "eval -client {} %{{ info -style modal -- {} }}\n",
            kak_quote(&self.client),
            kak_quote(message)
        ))
        .await
    }

    async fn show_info(&self, message: &str) -> Result<()> {
        self.send_to_kak(&format!(
            "eval -client {} %{{ info -style modal -- {} }}\n",
            kak_quote(&self.client),
            kak_quote(message)
        ))
        .await
    }

    async fn send_to_kak(&self, command: &str) -> Result<()> {
        let mut process = Command::new("kak");
        process
            .arg("-p")
            .arg(&self.session)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = process.spawn().context("failed to launch kak -p")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to access kak -p stdin"))?;
        stdin
            .write_all(command.as_bytes())
            .await
            .context("failed writing to kak -p")?;
        drop(stdin);
        let status = child.wait().await.context("failed to wait for kak -p")?;
        if !status.success() {
            return Err(anyhow!("kak -p exited with status {status}"));
        }
        Ok(())
    }
}

fn kak_quote(text: &str) -> String {
    let escaped = text.replace('\'', "''").replace('\n', "\\n");
    format!("'{}'", escaped)
}

struct KakouneAcpClient {
    dispatcher: Arc<KakouneDispatcher>,
}

impl KakouneAcpClient {
    fn new(dispatcher: Arc<KakouneDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[async_trait(?Send)]
impl acp::Client for KakouneAcpClient {
    async fn request_permission(
        &self,
        _args: acp::RequestPermissionRequest,
    ) -> anyhow::Result<acp::RequestPermissionResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn write_text_file(
        &self,
        _args: acp::WriteTextFileRequest,
    ) -> anyhow::Result<acp::WriteTextFileResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn read_text_file(
        &self,
        _args: acp::ReadTextFileRequest,
    ) -> anyhow::Result<acp::ReadTextFileResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn create_terminal(
        &self,
        _args: acp::CreateTerminalRequest,
    ) -> Result<acp::CreateTerminalResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn terminal_output(
        &self,
        _args: acp::TerminalOutputRequest,
    ) -> anyhow::Result<acp::TerminalOutputResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn release_terminal(
        &self,
        _args: acp::ReleaseTerminalRequest,
    ) -> anyhow::Result<acp::ReleaseTerminalResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn wait_for_terminal_exit(
        &self,
        _args: acp::WaitForTerminalExitRequest,
    ) -> anyhow::Result<acp::WaitForTerminalExitResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn kill_terminal_command(
        &self,
        _args: acp::KillTerminalCommandRequest,
    ) -> anyhow::Result<acp::KillTerminalCommandResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn session_notification(
        &self,
        args: acp::SessionNotification,
    ) -> anyhow::Result<(), acp::Error> {
        if let acp::SessionUpdate::AgentMessageChunk { content } = args.update {
            if let Some(text) = extract_text(&content) {
                if let Err(err) = self.dispatcher.append_agent_message(&text).await {
                    error!("failed to display agent output in Kakoune: {err:#}");
                }
            }
        }
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> Result<acp::ExtResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> Result<(), acp::Error> {
        Err(acp::Error::method_not_found())
    }
}

fn extract_text(content: &ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::ResourceLink(link) => Some(format!("Resource: {}", link.uri)),
        ContentBlock::Image(_) => Some("<image omitted>".to_string()),
        ContentBlock::Audio(_) => Some("<audio omitted>".to_string()),
        ContentBlock::Resource(_) => Some("<resource omitted>".to_string()),
    }
}

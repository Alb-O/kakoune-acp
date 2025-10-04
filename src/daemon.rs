use std::{ffi::OsString, path::PathBuf, sync::Arc};

use agent_client_protocol::{self as acp, Agent};
use anyhow::{Context, Result};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::Command,
    sync::{Mutex, Notify, broadcast},
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{
    cli::DaemonOptions,
    ipc::{self, DaemonRequest, DaemonResponse, PromptPayload, PromptResultPayload},
    kakoune,
    transcript::TranscriptCollector,
};

pub async fn run(options: DaemonOptions) -> Result<()> {
    let socket_path =
        kakoune::resolve_socket_path(options.socket.clone(), options.session.as_deref())?;
    let agent_command = options.agent.clone();
    let cwd = options.cwd.clone();

    let cleanup_path = socket_path.clone();
    let local_set = tokio::task::LocalSet::new();
    let result = local_set
        .run_until(async move { run_inner(socket_path, cwd, agent_command).await })
        .await;

    if cleanup_path.exists() {
        let _ = tokio::fs::remove_file(&cleanup_path).await;
    }

    result
}

async fn run_inner(
    socket_path: PathBuf,
    cwd: Option<PathBuf>,
    agent_command: Vec<OsString>,
) -> Result<()> {
    if agent_command.is_empty() {
        anyhow::bail!("no agent program provided");
    }

    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path)
            .await
            .with_context(|| {
                format!(
                    "failed to remove existing socket at {}",
                    socket_path.display()
                )
            })?;
    }

    let mut command = Command::new(&agent_command[0]);
    command.args(agent_command.iter().skip(1));
    if let Some(dir) = &cwd {
        command.current_dir(dir);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch agent {:?}", agent_command))?;

    let outgoing = child
        .stdin
        .take()
        .context("failed to open agent stdin")?
        .compat_write();
    let incoming = child
        .stdout
        .take()
        .context("failed to open agent stdout")?
        .compat();

    let (session_update_tx, _) = broadcast::channel(512);
    let client = KakouneClient::new(session_update_tx.clone());

    let (connection, io_task) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    let connection = Arc::new(connection);

    let shutdown_notify = Arc::new(Notify::new());
    {
        let shutdown = shutdown_notify.clone();
        tokio::task::spawn_local(async move {
            if let Err(err) = io_task.await {
                tracing::error!(?err, "agent IO loop terminated");
            }
            shutdown.notify_waiters();
        });
    }

    connection
        .initialize(acp::InitializeRequest {
            protocol_version: acp::V1,
            client_capabilities: acp::ClientCapabilities::default(),
            meta: None,
        })
        .await?;

    let cwd = if let Some(cwd) = cwd {
        cwd
    } else {
        std::env::current_dir()?
    };

    let session_response = connection
        .new_session(acp::NewSessionRequest {
            cwd,
            mcp_servers: Vec::new(),
            meta: None,
        })
        .await?;

    let session_id = session_response.session_id.clone();
    let status = ipc::DaemonStatus {
        session_id: Some(session_id.to_string()),
        socket_path: socket_path.clone(),
        agent_command: agent_command
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        agent_pid: child.id(),
        running: true,
    };
    let status = Arc::new(Mutex::new(status));

    let state = Arc::new(InnerState {
        connection: connection.clone(),
        session_id: session_id.clone(),
        updates: session_update_tx,
        shutdown: shutdown_notify.clone(),
        status: status.clone(),
    });

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind socket at {}", socket_path.display()))?;
    tracing::info!("daemon listening on {}", socket_path.display());

    loop {
        tokio::select! {
            _ = shutdown_notify.notified() => {
                tracing::info!("shutdown requested");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::task::spawn_local(async move {
                            if let Err(err) = handle_connection(stream, state).await {
                                tracing::warn!(?err, "client connection failed");
                            }
                        });
                    }
                    Err(err) => {
                        tracing::error!(?err, "failed to accept connection");
                        break;
                    }
                }
            }
        }
    }

    {
        let mut status = status.lock().await;
        status.running = false;
    }

    if let Err(err) = child.start_kill() {
        tracing::debug!(?err, "failed to signal agent for shutdown");
    }
    let _ = child.wait().await;

    drop(listener);
    Ok(())
}

async fn handle_connection(stream: UnixStream, state: Arc<InnerState>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(());
    }
    let line = line.trim_end();
    let request: DaemonRequest =
        serde_json::from_str(line).with_context(|| format!("failed to parse request: {line}"))?;

    let response = match request {
        DaemonRequest::Prompt(payload) => match state.run_prompt(payload).await {
            Ok(result) => DaemonResponse::Prompt { result },
            Err(error) => {
                tracing::error!(?error, "prompt handling failed");
                DaemonResponse::Error {
                    message: error.to_string(),
                }
            }
        },
        DaemonRequest::Status => {
            let status = { state.status.lock().await.clone() };
            DaemonResponse::Status { status }
        }
        DaemonRequest::Shutdown => {
            {
                let mut status = state.status.lock().await;
                status.running = false;
            }
            state.shutdown.notify_waiters();
            DaemonResponse::Ok
        }
    };

    let payload = serde_json::to_string(&response)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

struct InnerState {
    connection: Arc<acp::ClientSideConnection>,
    session_id: acp::SessionId,
    updates: broadcast::Sender<acp::SessionNotification>,
    shutdown: Arc<Notify>,
    status: Arc<Mutex<ipc::DaemonStatus>>,
}

impl InnerState {
    async fn run_prompt(&self, payload: PromptPayload) -> Result<PromptResultPayload> {
        let PromptPayload { prompt, context } = payload;
        let mut collector = TranscriptCollector::new();
        collector.push_user_prompt(prompt.clone());

        let mut prompt_blocks = Vec::new();
        prompt_blocks.push(acp::ContentBlock::from(prompt.clone()));
        for snippet in &context {
            prompt_blocks.push(acp::ContentBlock::from(snippet.text.clone()));
        }

        let mut updates = self.updates.subscribe();
        let mut prompt_future = Box::pin(self.connection.prompt(acp::PromptRequest {
            session_id: self.session_id.clone(),
            prompt: prompt_blocks,
            meta: Some(json!({
                "source": "kakoune",
            })),
        }));

        loop {
            tokio::select! {
                update = updates.recv() => {
                    match update {
                        Ok(notification) => {
                            if notification.session_id == self.session_id {
                                collector.record_notification(notification);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "dropped {skipped} session notifications");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            anyhow::bail!("session notification channel closed");
                        }
                    }
                }
                response = &mut prompt_future => {
                    let response = response?;
                    return Ok(PromptResultPayload {
                        stop_reason: response.stop_reason,
                        user_prompt: prompt,
                        context,
                        transcript: collector.finish(),
                    });
                }
            }
        }
    }
}

struct KakouneClient {
    updates: broadcast::Sender<acp::SessionNotification>,
}

impl KakouneClient {
    fn new(updates: broadcast::Sender<acp::SessionNotification>) -> Self {
        Self { updates }
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for KakouneClient {
    async fn request_permission(
        &self,
        _args: acp::RequestPermissionRequest,
    ) -> Result<acp::RequestPermissionResponse, acp::Error> {
        Ok(acp::RequestPermissionResponse {
            outcome: acp::RequestPermissionOutcome::Cancelled,
            meta: Some(json!({
                "reason": "permission UI not implemented in kakoune-acp",
            })),
        })
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> Result<(), acp::Error> {
        let _ = self.updates.send(args);
        Ok(())
    }
}

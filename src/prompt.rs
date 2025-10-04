use std::io::IsTerminal;

use anyhow::{Context, Result, anyhow};
use tokio::io::AsyncReadExt;

use crate::{
    cli::{PromptOptions, PromptOutput},
    ipc::{self, DaemonResponse, PromptPayload, PromptResultPayload, TranscriptEvent},
    ipc_client, kakoune,
};

pub async fn run(options: PromptOptions) -> Result<()> {
    let socket_path =
        kakoune::resolve_socket_path(options.socket.clone(), options.session.as_deref())?;
    let prompt_text = read_prompt(&options).await?;

    if prompt_text.trim().is_empty() {
        return Err(anyhow!("prompt is empty"));
    }

    let context_snippets = read_context_snippets(&options).await?;
    let payload = PromptPayload {
        prompt: prompt_text.clone(),
        context: context_snippets,
    };

    let response =
        ipc_client::roundtrip(&socket_path, &ipc::DaemonRequest::Prompt(payload)).await?;
    match response {
        DaemonResponse::Prompt { result } => handle_prompt_result(&options, result).await?,
        DaemonResponse::Error { message } => return Err(anyhow!(message)),
        other => {
            return Err(anyhow!(format!(
                "unexpected response from daemon: {other:?}"
            )));
        }
    }

    Ok(())
}

async fn read_prompt(options: &PromptOptions) -> Result<String> {
    let prompt_from_stdin = options.prompt.is_none() && options.prompt_file.is_none();

    if prompt_from_stdin && std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "no prompt provided; pass --prompt, --prompt-file, or pipe text on stdin"
        ));
    }

    if let Some(prompt) = &options.prompt {
        return Ok(prompt.clone());
    }
    if let Some(path) = &options.prompt_file {
        return tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read prompt file {}", path.display()));
    }
    let mut buffer = String::new();
    tokio::io::stdin()
        .read_to_string(&mut buffer)
        .await
        .context("failed to read prompt from stdin")?;
    Ok(buffer)
}

async fn read_context_snippets(options: &PromptOptions) -> Result<Vec<String>> {
    let mut snippets = options.context.clone();
    for path in &options.context_files {
        let contents = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read context file {}", path.display()))?;
        snippets.push(contents);
    }
    Ok(snippets)
}

async fn handle_prompt_result(options: &PromptOptions, result: PromptResultPayload) -> Result<()> {
    let plain_text = render_plain_text(&result);

    match options.output {
        PromptOutput::Plain => {
            print!("{}", plain_text);
            if !plain_text.ends_with('\n') {
                println!();
            }
            if options.send_to_kak {
                send_to_kakoune(options, &plain_text).await?;
            }
        }
        PromptOutput::Json => {
            let json = serde_json::to_string_pretty(&result)?;
            println!("{}", json);
            if options.send_to_kak {
                send_to_kakoune(options, &plain_text).await?;
            }
        }
        PromptOutput::KakCommands => {
            let command = kakoune::format_info_command(
                options.client.as_deref(),
                &options.title,
                &plain_text,
            );
            if options.send_to_kak {
                send_to_kakoune(options, &plain_text).await?;
            } else {
                print!("{}", command);
            }
        }
    }

    Ok(())
}

async fn send_to_kakoune(options: &PromptOptions, body: &str) -> Result<()> {
    let session = options
        .session
        .as_deref()
        .ok_or_else(|| anyhow!("--send-to-kak requires a Kakoune session (set kak_session)"))?;
    let command = kakoune::format_info_command(options.client.as_deref(), &options.title, body);
    kakoune::send_to_kak(session, &command)
}

fn render_plain_text(result: &PromptResultPayload) -> String {
    let mut output = String::new();
    output.push_str("=== Prompt ===\n");
    output.push_str(result.user_prompt.trim_end());
    output.push_str("\n");
    if !result.context.is_empty() {
        for (index, snippet) in result.context.iter().enumerate() {
            output.push_str(&format!("\n[context #{}]\n", index + 1));
            output.push_str(snippet);
            if !snippet.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output.push('\n');

    for event in &result.transcript {
        match event {
            TranscriptEvent::UserMessage { text } => {
                output.push_str("[user] ");
                output.push_str(text);
                output.push('\n');
            }
            TranscriptEvent::AgentMessage { text } => {
                output.push_str("[agent] ");
                output.push_str(text);
                output.push('\n');
            }
            TranscriptEvent::AgentThought { text } => {
                output.push_str("[thought] ");
                output.push_str(text);
                output.push('\n');
            }
            TranscriptEvent::ToolCall { id, title, status } => {
                output.push_str(&format!("[tool {id}] {status}: {title}\n"));
            }
            TranscriptEvent::ToolCallUpdate {
                id,
                status,
                message,
            } => {
                let status = status.as_deref().map(|s| s.as_ref()).unwrap_or("update");
                output.push_str(&format!("[tool {id}] {status}\n"));
                if let Some(message) = message {
                    output.push_str(message);
                    output.push('\n');
                }
            }
            TranscriptEvent::Plan { entries } => {
                output.push_str("[plan]\n");
                for entry in entries {
                    output.push_str(&format!(
                        "  - ({}/{}) {}\n",
                        entry.status, entry.priority, entry.content
                    ));
                }
            }
            TranscriptEvent::AvailableCommands { commands } => {
                output.push_str("[commands]\n");
                for command in commands {
                    output.push_str(&format!("  - {}: {}\n", command.name, command.description));
                    if let Some(hint) = &command.hint {
                        output.push_str(&format!("      hint: {}\n", hint));
                    }
                }
            }
            TranscriptEvent::SystemMessage { text } => {
                output.push_str(&format!("[system] {}\n", text));
            }
        }
    }

    output.push_str(&format!("\nStop reason: {:?}\n", result.stop_reason));
    output
}

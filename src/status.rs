use anyhow::{anyhow, Result};
use serde_json;

use crate::{
    cli::{ShutdownOptions, StatusOptions},
    ipc::{self, DaemonResponse},
    ipc_client, kakoune,
};

pub async fn run_status(options: StatusOptions) -> Result<()> {
    let socket_path =
        kakoune::resolve_socket_path(options.socket.clone(), options.session.as_deref())?;
    let response = ipc_client::roundtrip(&socket_path, &ipc::DaemonRequest::Status).await?;
    match response {
        DaemonResponse::Status { status } => {
            if options.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Socket: {}", status.socket_path.display());
                if let Some(session) = status.session_id {
                    println!("Session ID: {}", session);
                }
                println!("Agent running: {}", status.running);
                if let Some(pid) = status.agent_pid {
                    println!("Agent PID: {}", pid);
                }
                if !status.agent_command.is_empty() {
                    println!("Agent command: {}", status.agent_command.join(" "));
                }
            }
        }
        DaemonResponse::Error { message } => return Err(anyhow!(message)),
        other => return Err(anyhow!(format!("unexpected daemon response: {other:?}"))),
    }
    Ok(())
}

pub async fn run_shutdown(options: ShutdownOptions) -> Result<()> {
    let socket_path =
        kakoune::resolve_socket_path(options.socket.clone(), options.session.as_deref())?;
    let response = ipc_client::roundtrip(&socket_path, &ipc::DaemonRequest::Shutdown).await?;
    match response {
        DaemonResponse::Ok => {
            println!("daemon shut down");
        }
        DaemonResponse::Error { message } => return Err(anyhow!(message)),
        other => return Err(anyhow!(format!("unexpected daemon response: {other:?}"))),
    }
    Ok(())
}

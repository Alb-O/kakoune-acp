use std::{cell::Cell, time::Duration};

use agent_client_protocol::{self as acp, Client};
use anyhow::Result;
use tokio::{
    sync::{mpsc, oneshot},
    time::sleep,
};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

struct MockAgent {
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    next_session_id: Cell<u64>,
}

impl MockAgent {
    fn new(
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    ) -> Self {
        Self {
            session_update_tx,
            next_session_id: Cell::new(0),
        }
    }

    async fn send_update(
        &self,
        session_id: &acp::SessionId,
        update: acp::SessionUpdate,
    ) -> std::result::Result<(), acp::Error> {
        let (tx, rx) = oneshot::channel();
        self.session_update_tx
            .send((
                acp::SessionNotification {
                    session_id: session_id.clone(),
                    update,
                    meta: None,
                },
                tx,
            ))
            .map_err(|_| acp::Error::internal_error())?;
        rx.await.map_err(|_| acp::Error::internal_error())
    }
}

fn summarize_prompt_blocks(blocks: &[acp::ContentBlock]) -> String {
    let mut summary = Vec::new();
    for block in blocks {
        if let acp::ContentBlock::Text(text) = block {
            if !text.text.trim().is_empty() {
                summary.push(text.text.trim().to_string());
            }
        }
    }
    if summary.is_empty() {
        "(no text provided)".to_string()
    } else {
        summary.join(" ")
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for MockAgent {
    async fn initialize(
        &self,
        _: acp::InitializeRequest,
    ) -> std::result::Result<acp::InitializeResponse, acp::Error> {
        Ok(acp::InitializeResponse {
            protocol_version: acp::V1,
            agent_capabilities: acp::AgentCapabilities::default(),
            auth_methods: Vec::new(),
            meta: None,
        })
    }

    async fn authenticate(
        &self,
        _: acp::AuthenticateRequest,
    ) -> std::result::Result<acp::AuthenticateResponse, acp::Error> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        _: acp::NewSessionRequest,
    ) -> std::result::Result<acp::NewSessionResponse, acp::Error> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(session_id.to_string().into()),
            modes: None,
            meta: None,
        })
    }

    async fn prompt(
        &self,
        arguments: acp::PromptRequest,
    ) -> std::result::Result<acp::PromptResponse, acp::Error> {
        let session_id = arguments.session_id.clone();
        let summary = summarize_prompt_blocks(&arguments.prompt);

        self.send_update(
            &session_id,
            acp::SessionUpdate::AgentThoughtChunk {
                content: format!("Thinking about: {summary}").into(),
            },
        )
        .await?;

        self.send_update(
            &session_id,
            acp::SessionUpdate::Plan(acp::Plan {
                entries: vec![
                    acp::PlanEntry {
                        content: "Read the provided context".into(),
                        priority: acp::PlanEntryPriority::High,
                        status: acp::PlanEntryStatus::InProgress,
                        meta: None,
                    },
                    acp::PlanEntry {
                        content: "Draft a helpful response".into(),
                        priority: acp::PlanEntryPriority::Medium,
                        status: acp::PlanEntryStatus::Pending,
                        meta: None,
                    },
                ],
                meta: None,
            }),
        )
        .await?;

        self.send_update(
            &session_id,
            acp::SessionUpdate::AvailableCommandsUpdate {
                available_commands: vec![acp::AvailableCommand {
                    name: "apply_suggestion".into(),
                    description: "Apply the generated response to the buffer".into(),
                    input: Some(acp::AvailableCommandInput::Unstructured {
                        hint: "Type edits that should be applied".into(),
                    }),
                    meta: None,
                }],
            },
        )
        .await?;

        let tool_id = acp::ToolCallId("write_summary".into());
        self.send_update(
            &session_id,
            acp::SessionUpdate::ToolCall(acp::ToolCall {
                id: tool_id.clone(),
                title: "Generate summary".into(),
                kind: acp::ToolKind::Edit,
                status: acp::ToolCallStatus::InProgress,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                raw_output: None,
                meta: None,
            }),
        )
        .await?;

        self.send_update(
            &session_id,
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate {
                id: tool_id.clone(),
                fields: acp::ToolCallUpdateFields {
                    status: Some(acp::ToolCallStatus::Completed),
                    content: Some(vec![acp::ToolCallContent::from(format!(
                        "Summary created for: {summary}"
                    ))]),
                    title: Some("Generated summary".into()),
                    ..Default::default()
                },
                meta: None,
            }),
        )
        .await?;

        self.send_update(
            &session_id,
            acp::SessionUpdate::CurrentModeUpdate {
                current_mode_id: acp::SessionModeId("writer".into()),
            },
        )
        .await?;

        self.send_update(
            &session_id,
            acp::SessionUpdate::AgentMessageChunk {
                content: "Here is your concise summary.".into(),
            },
        )
        .await?;

        sleep(Duration::from_millis(50)).await;

        Ok(acp::PromptResponse {
            stop_reason: acp::StopReason::EndTurn,
            meta: None,
        })
    }

    async fn cancel(&self, _: acp::CancelNotification) -> std::result::Result<(), acp::Error> {
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let (connection, io_task) =
                acp::AgentSideConnection::new(MockAgent::new(tx), outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            tokio::task::spawn_local(async move {
                while let Some((notification, ack)) = rx.recv().await {
                    if connection.session_notification(notification).await.is_err() {
                        break;
                    }
                    let _ = ack.send(());
                }
            });

            io_task.await
        })
        .await?;
    Ok(())
}

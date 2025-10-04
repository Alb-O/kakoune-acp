use std::cell::Cell;

use agent_client_protocol::{
    self as acp, Agent, AvailableCommand, AvailableCommandInput, Client, Plan, PlanEntry,
    PlanEntryPriority, PlanEntryStatus, SessionMode, SessionModeId, SessionModeState,
    SessionNotification, SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use anyhow::Result;
use serde_json::json;
use tokio::{
    sync::{mpsc, oneshot},
    task::yield_now,
    time::{Duration, sleep},
};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

struct MockAgent {
    session_update_tx: mpsc::UnboundedSender<(SessionNotification, oneshot::Sender<()>)>,
    next_session_id: Cell<u64>,
}

impl MockAgent {
    fn new(
        session_update_tx: mpsc::UnboundedSender<(SessionNotification, oneshot::Sender<()>)>,
    ) -> Self {
        Self {
            session_update_tx,
            next_session_id: Cell::new(0),
        }
    }

    async fn send_update(
        &self,
        session_id: acp::SessionId,
        update: SessionUpdate,
    ) -> Result<(), acp::Error> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.session_update_tx
            .send((
                SessionNotification {
                    session_id,
                    update,
                    meta: None,
                },
                ack_tx,
            ))
            .map_err(|_| acp::Error::internal_error())?;
        ack_rx.await.map_err(|_| acp::Error::internal_error())?;
        yield_now().await;
        sleep(Duration::from_millis(10)).await;
        Ok(())
    }
}

fn render_content(block: &acp::ContentBlock) -> String {
    match block {
        acp::ContentBlock::Text(text) => text.text.clone(),
        acp::ContentBlock::Image(image) => image
            .uri
            .clone()
            .unwrap_or_else(|| format!("<image:{}>", image.mime_type)),
        acp::ContentBlock::Audio(audio) => format!("<audio:{}>", audio.mime_type),
        acp::ContentBlock::ResourceLink(link) => link
            .description
            .clone()
            .or_else(|| link.title.clone())
            .or_else(|| Some(link.name.clone()))
            .unwrap_or_else(|| link.uri.clone()),
        acp::ContentBlock::Resource(resource) => match &resource.resource {
            acp::EmbeddedResourceResource::TextResourceContents(text) => text.text.clone(),
            acp::EmbeddedResourceResource::BlobResourceContents(blob) => {
                format!("<resource:{}>", blob.uri)
            }
        },
    }
}

#[async_trait::async_trait(?Send)]
impl Agent for MockAgent {
    async fn initialize(
        &self,
        _arguments: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
        Ok(acp::InitializeResponse {
            protocol_version: acp::V1,
            agent_capabilities: acp::AgentCapabilities::default(),
            auth_methods: Vec::new(),
            meta: None,
        })
    }

    async fn authenticate(
        &self,
        _arguments: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse, acp::Error> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        _arguments: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(session_id.to_string().into()),
            modes: Some(SessionModeState {
                current_mode_id: SessionModeId("demo-mode".into()),
                available_modes: vec![SessionMode {
                    id: SessionModeId("demo-mode".into()),
                    name: "Demo Mode".into(),
                    description: Some("A lightweight mode that streams rich updates.".into()),
                    meta: None,
                }],
                meta: None,
            }),
            meta: Some(json!({ "workspace": "mock" })),
        })
    }

    async fn prompt(
        &self,
        arguments: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        let session_id = arguments.session_id.clone();
        let mut blocks = arguments.prompt.iter();
        let user_prompt = blocks
            .next()
            .map(render_content)
            .unwrap_or_else(|| "<empty prompt>".into());
        let context_snippets: Vec<String> = blocks.map(render_content).collect();

        self.send_update(
            session_id.clone(),
            SessionUpdate::UserMessageChunk {
                content: format!("{user_prompt}").into(),
            },
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::AgentThoughtChunk {
                content: format!(
                    "Analysing {} context snippets for `{user_prompt}`",
                    context_snippets.len()
                )
                .into(),
            },
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::Plan(Plan {
                entries: vec![
                    PlanEntry {
                        content: "Understand the prompt".into(),
                        priority: PlanEntryPriority::High,
                        status: PlanEntryStatus::InProgress,
                        meta: None,
                    },
                    PlanEntry {
                        content: "Synthesize a response".into(),
                        priority: PlanEntryPriority::Medium,
                        status: PlanEntryStatus::Pending,
                        meta: None,
                    },
                ],
                meta: None,
            }),
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::AvailableCommandsUpdate {
                available_commands: vec![AvailableCommand {
                    name: "refine_response".into(),
                    description: "Ask the agent to iterate on the generated answer".into(),
                    input: Some(AvailableCommandInput::Unstructured {
                        hint: "Describe the additional detail you need".into(),
                    }),
                    meta: None,
                }],
            },
        )
        .await?;

        let tool_id = ToolCallId("demo-tool".into());
        self.send_update(
            session_id.clone(),
            SessionUpdate::ToolCall(ToolCall {
                id: tool_id.clone(),
                title: "Simulate execution".into(),
                kind: acp::ToolKind::Execute,
                status: ToolCallStatus::InProgress,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: Some(json!({
                    "prompt": user_prompt,
                    "context": context_snippets,
                })),
                raw_output: None,
                meta: None,
            }),
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::ToolCallUpdate(ToolCallUpdate {
                id: tool_id.clone(),
                fields: ToolCallUpdateFields {
                    status: Some(ToolCallStatus::Completed),
                    content: Some(vec![ToolCallContent::from(
                        "Executed synthetic task for the prompt",
                    )]),
                    ..Default::default()
                },
                meta: None,
            }),
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::CurrentModeUpdate {
                current_mode_id: SessionModeId("demo-mode".into()),
            },
        )
        .await?;
        self.send_update(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk {
                content: "I am preparing your answer.".into(),
            },
        )
        .await?;
        self.send_update(
            session_id,
            SessionUpdate::AgentMessageChunk {
                content: format!(
                    "Processed `{user_prompt}` with {} extra snippets.",
                    arguments.prompt.len().saturating_sub(1)
                )
                .into(),
            },
        )
        .await?;

        Ok(acp::PromptResponse {
            stop_reason: StopReason::EndTurn,
            meta: Some(json!({
                "context_snippets": arguments.prompt.len().saturating_sub(1),
            })),
        })
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> Result<(), acp::Error> {
        Ok(())
    }

    async fn load_session(
        &self,
        _args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        Ok(acp::LoadSessionResponse {
            modes: Some(SessionModeState {
                current_mode_id: SessionModeId("demo-mode".into()),
                available_modes: vec![SessionMode {
                    id: SessionModeId("demo-mode".into()),
                    name: "Demo Mode".into(),
                    description: Some("Mock agent load stub".into()),
                    meta: None,
                }],
                meta: None,
            }),
            meta: None,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let (conn, io_task) = acp::AgentSideConnection::new(
                MockAgent::new(tx.clone()),
                outgoing,
                incoming,
                |fut| {
                    tokio::task::spawn_local(fut);
                },
            );

            tokio::task::spawn_local(async move {
                while let Some((notification, ack)) = rx.recv().await {
                    let result = conn.session_notification(notification).await;
                    if let Err(err) = result {
                        eprintln!("mock agent failed to send notification: {err}");
                        let _ = ack.send(());
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

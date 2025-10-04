use agent_client_protocol as acp;

use crate::ipc::{CommandSummary, PlanEntrySummary, TranscriptEvent};

pub struct TranscriptCollector {
    events: Vec<TranscriptEvent>,
}

impl TranscriptCollector {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn push_user_prompt(&mut self, text: String) {
        if !text.is_empty() {
            self.events.push(TranscriptEvent::UserMessage { text });
        }
    }

    pub fn record_notification(&mut self, notification: acp::SessionNotification) {
        use acp::SessionUpdate;

        match notification.update {
            SessionUpdate::AgentMessageChunk { content } => {
                self.events.push(TranscriptEvent::AgentMessage {
                    text: render_content(content),
                });
            }
            SessionUpdate::AgentThoughtChunk { content } => {
                self.events.push(TranscriptEvent::AgentThought {
                    text: render_content(content),
                });
            }
            SessionUpdate::UserMessageChunk { content } => {
                self.events.push(TranscriptEvent::UserMessage {
                    text: render_content(content),
                });
            }
            SessionUpdate::ToolCall(tool_call) => {
                self.events.push(TranscriptEvent::ToolCall {
                    id: tool_call.id.0.to_string(),
                    title: tool_call.title,
                    status: format!("{:?}", tool_call.status),
                });
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.events.push(summarize_tool_call_update(update));
            }
            SessionUpdate::Plan(plan) => {
                let entries = plan
                    .entries
                    .into_iter()
                    .map(|entry| PlanEntrySummary {
                        status: format!("{:?}", entry.status),
                        priority: format!("{:?}", entry.priority),
                        content: entry.content,
                    })
                    .collect();
                self.events.push(TranscriptEvent::Plan { entries });
            }
            SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                let commands = available_commands
                    .into_iter()
                    .map(|command| CommandSummary {
                        name: command.name,
                        description: command.description,
                        hint: match command.input {
                            Some(acp::AvailableCommandInput::Unstructured { hint }) => Some(hint),
                            None => None,
                        },
                    })
                    .collect();
                self.events
                    .push(TranscriptEvent::AvailableCommands { commands });
            }
            SessionUpdate::CurrentModeUpdate { current_mode_id } => {
                self.events.push(TranscriptEvent::SystemMessage {
                    text: format!("Current mode: {}", current_mode_id.0),
                });
            }
        }
    }

    pub fn finish(self) -> Vec<TranscriptEvent> {
        self.events
    }
}

fn render_content(block: acp::ContentBlock) -> String {
    match block {
        acp::ContentBlock::Text(text) => text.text,
        acp::ContentBlock::Image(image) => image
            .uri
            .unwrap_or_else(|| format!("<image:{}>", image.mime_type)),
        acp::ContentBlock::Audio(audio) => format!("<audio:{}>", audio.mime_type),
        acp::ContentBlock::ResourceLink(link) => link
            .description
            .or(link.title)
            .unwrap_or_else(|| format!("<resource:{}>", link.uri)),
        acp::ContentBlock::Resource(resource) => match resource.resource {
            acp::EmbeddedResourceResource::TextResourceContents(text) => text.text,
            acp::EmbeddedResourceResource::BlobResourceContents(blob) => {
                format!("<resource:{}>", blob.uri)
            }
        },
    }
}

fn summarize_tool_call_update(update: acp::ToolCallUpdate) -> TranscriptEvent {
    let status = update.fields.status.map(|status| format!("{:?}", status));
    let mut message_parts = Vec::new();
    if let Some(title) = update.fields.title.clone() {
        message_parts.push(title);
    }
    if let Some(content) = update.fields.content.clone() {
        for entry in content {
            message_parts.push(match entry {
                acp::ToolCallContent::Content { content } => render_content(content),
                acp::ToolCallContent::Diff { diff } => {
                    format!("diff for {}", diff.path.display())
                }
                acp::ToolCallContent::Terminal { terminal_id } => {
                    format!("terminal {}", terminal_id.0)
                }
            });
        }
    }
    let message = if message_parts.is_empty() {
        None
    } else {
        Some(message_parts.join("\n"))
    };
    TranscriptEvent::ToolCallUpdate {
        id: update.id.0.to_string(),
        status,
        message,
    }
}

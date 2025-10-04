use std::path::PathBuf;

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonRequest {
    Prompt(PromptPayload),
    Status,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPayload {
    pub prompt: String,
    #[serde(default)]
    pub context: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonResponse {
    Prompt { result: PromptResultPayload },
    Status { status: DaemonStatus },
    Ok,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResultPayload {
    pub stop_reason: acp::StopReason,
    pub user_prompt: String,
    #[serde(default)]
    pub context: Vec<String>,
    pub transcript: Vec<TranscriptEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub session_id: Option<String>,
    pub socket_path: PathBuf,
    pub agent_command: Vec<String>,
    pub agent_pid: Option<u32>,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEvent {
    UserMessage {
        text: String,
    },
    AgentMessage {
        text: String,
    },
    AgentThought {
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        id: String,
        status: Option<String>,
        message: Option<String>,
    },
    Plan {
        entries: Vec<PlanEntrySummary>,
    },
    AvailableCommands {
        commands: Vec<CommandSummary>,
    },
    SystemMessage {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntrySummary {
    pub status: String,
    pub priority: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSummary {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

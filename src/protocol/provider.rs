use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct AiCapabilities {
    pub vision: bool,
    pub tool_calling: bool,
    pub structured_output: bool,
    pub streaming: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiPermission {
    #[default]
    Observe,
    Suggest,
    Assist,
    Autonomous,
}

/// Who produced a turn in a conversation. The editor keeps a short transcript so a model can see
/// what it already proposed, without ever seeing the whole video (AI-TOOLS.md).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiRole {
    User,
    Assistant,
}

impl AiRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "あなた",
            Self::Assistant => "AI",
        }
    }

    /// OpenAI-compatible `role` string.
    pub fn api_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AiMessage {
    pub role: AiRole,
    pub content: String,
}

impl AiMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: AiRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: AiRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AiRequest {
    pub system_context: String,
    /// Earlier turns, oldest first, so multi-turn conversation works over a stateless HTTP API.
    pub history: Vec<AiMessage>,
    pub user_message: String,
    pub permission: AiPermission,
}

#[derive(Clone, Debug)]
pub struct AiResponse {
    pub message: String,
    pub structured_aep: Option<String>,
}

#[derive(Debug, Error)]
pub enum AiError {
    #[error("provider is not configured")]
    NotConfigured,
    #[error("provider request failed: {0}")]
    Request(String),
    #[error("provider returned invalid structured output: {0}")]
    InvalidOutput(String),
}

/// Provider-specific schemas terminate at this interface. Editing code consumes only AEP.
pub trait AiProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> AiCapabilities;
    fn send(&self, request: AiRequest) -> Result<AiResponse, AiError>;

    /// One chat turn in which the model may call tools. Providers that cannot do tool calling keep
    /// the default, so the agent degrades to plain chat instead of failing to compile.
    fn send_tools(&self, _request: AiToolRequest) -> Result<AiToolResponse, AiError> {
        Err(AiError::Request(
            "このプロバイダはツール呼び出しに対応していません".to_owned(),
        ))
    }
}

/// A tool the agent may call, advertised to the model as a JSON-Schema function.
#[derive(Clone, Debug)]
pub struct AiToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

impl AiToolSpec {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters,
        }
    }
}

/// A tool call requested by the model. `arguments` is the raw JSON string the model produced; the
/// agent parses it so a malformed call is reported back to the model instead of crashing.
#[derive(Clone, Debug, PartialEq)]
pub struct AiToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// An inline image sent to a vision-capable model, base64-encoded with its MIME type.
#[derive(Clone, Debug, PartialEq)]
pub struct AiImage {
    pub mime: String,
    pub base64: String,
}

impl AiImage {
    pub fn jpeg(base64: String) -> Self {
        Self {
            mime: "image/jpeg".to_owned(),
            base64,
        }
    }

    /// The `data:` URL an OpenAI-compatible vision request expects.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.base64)
    }
}

/// One turn of an agent conversation. Richer than [`AiMessage`] because assistant turns can carry
/// tool calls, tool turns must reference the call they answer, and a turn can show the model a
/// frame (vision).
#[derive(Clone, Debug)]
pub enum AiTurn {
    User(String),
    Assistant {
        content: String,
        tool_calls: Vec<AiToolCall>,
    },
    Tool {
        call_id: String,
        name: String,
        content: String,
    },
    /// A user turn that may carry images — how the editor lets a vision model look at a frame.
    Images {
        text: String,
        images: Vec<AiImage>,
    },
}

impl AiTurn {
    pub fn user(content: impl Into<String>) -> Self {
        Self::User(content.into())
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn tool(
        call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::Tool {
            call_id: call_id.into(),
            name: name.into(),
            content: content.into(),
        }
    }

    pub fn images(text: impl Into<String>, images: Vec<AiImage>) -> Self {
        Self::Images {
            text: text.into(),
            images,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AiToolRequest {
    pub system_context: String,
    pub turns: Vec<AiTurn>,
    pub tools: Vec<AiToolSpec>,
    pub permission: AiPermission,
}

#[derive(Clone, Debug)]
pub struct AiToolResponse {
    /// Final text of the turn. Empty when the model only asked for tools.
    pub content: String,
    pub tool_calls: Vec<AiToolCall>,
}

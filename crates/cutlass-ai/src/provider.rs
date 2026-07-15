//! The provider seam: chat completion with tool calling, behind one trait.
//!
//! Blocking by design — the agent runs on its own thread (never the UI
//! thread), and a synchronous trait keeps tokio out of the app. Streaming
//! is a text callback (for the chat panel) plus a completed [`ChatTurn`]
//! return; tool calls arrive whole, in the turn.

use std::sync::atomic::AtomicBool;

use crate::wire::ToolSpec;

/// An image attached to a message. Raw encoded bytes (PNG or JPEG) —
/// base64 encoding happens at the provider boundary, never earlier.
/// Images are per-turn working memory: the runtime budgets them per
/// request and strips them from session history (see agent.rs).
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePart {
    /// MIME type: "image/png" or "image/jpeg".
    pub media_type: String,
    /// Raw encoded bytes, shared so message clones stay cheap.
    pub data: std::sync::Arc<Vec<u8>>,
    /// Short human label for transcripts and placeholders, e.g. "timeline at 12.40s".
    pub label: String,
}

impl ImagePart {
    pub fn png(data: Vec<u8>, label: impl Into<String>) -> Self {
        Self {
            media_type: "image/png".to_string(),
            data: std::sync::Arc::new(data),
            label: label.into(),
        }
    }

    pub fn jpeg(data: Vec<u8>, label: impl Into<String>) -> Self {
        Self {
            media_type: "image/jpeg".to_string(),
            data: std::sync::Arc::new(data),
            label: label.into(),
        }
    }
}

/// One entry in the conversation, provider-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        images: Vec<ImagePart>,
    },
    /// A prior model turn (text and/or the tool calls it made).
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    /// The outcome of one tool call, fed back to the model.
    ToolResult {
        call_id: String,
        content: String,
        images: Vec<ImagePart>,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
            images: Vec::new(),
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            call_id: call_id.into(),
            content: content.into(),
            images: Vec::new(),
        }
    }
}

/// A tool invocation the model requested.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider-assigned id; echoed back in the matching [`Message::ToolResult`].
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Natural end of a text answer.
    Stop,
    /// The model wants its tool calls executed.
    ToolCalls,
    /// Token limit hit; the turn is truncated.
    Length,
    Other,
}

/// One completed model turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
}

/// Everything a provider needs for one completion.
pub struct ChatRequest<'a> {
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
}

/// Provider failures, kept distinct so the UI can say "Ollama isn't
/// running at localhost:11434" instead of "something failed".
#[derive(Debug)]
pub enum ProviderError {
    /// No `[ai]` config, or it is unusable (missing key, bad env var).
    NotConfigured(String),
    /// Could not reach the endpoint at all.
    Network(String),
    /// The endpoint answered with an error (HTTP status, rate limit, …).
    Provider { status: u16, message: String },
    /// The endpoint answered with something we could not parse.
    Protocol(String),
    /// The cancel flag was raised mid-stream.
    Cancelled,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(msg) => write!(f, "AI is not configured: {msg}"),
            Self::Network(msg) => write!(f, "could not reach the AI provider: {msg}"),
            Self::Provider { status, message } => {
                write!(f, "the AI provider returned HTTP {status}: {message}")
            }
            Self::Protocol(msg) => write!(f, "unexpected response from the AI provider: {msg}"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Chat completion with tool calling and streamed text.
///
/// Implementations must check `cancel` between chunks and return
/// [`ProviderError::Cancelled`] promptly when it goes true. `on_text`
/// receives assistant text deltas as they stream.
pub trait ChatProvider {
    fn chat(
        &self,
        request: &ChatRequest<'_>,
        cancel: &AtomicBool,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<ChatTurn, ProviderError>;
}

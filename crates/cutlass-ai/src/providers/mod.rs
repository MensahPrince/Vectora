//! Provider implementations behind the [`crate::provider::ChatProvider`] seam.

use std::sync::atomic::AtomicBool;

use crate::provider::{ChatProvider, ChatRequest, ChatTurn, ProviderError, ProviderStreamEvent};

pub mod openai_compat;
pub mod openai_responses;
pub mod scripted;

pub use openai_compat::OpenAiCompatProvider;
pub use openai_responses::OpenAiResponsesProvider;
pub use scripted::ScriptedProvider;

/// Explicit wire protocol for an OpenAI-style endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiProtocol {
    ChatCompletions,
    Responses,
}

/// Provider-safe reasoning visibility. Raw chain-of-thought is never exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningSummary {
    Auto,
    Off,
}

/// Runtime protocol dispatcher used by desktop settings. Keeping this wrapper
/// outside either transport prevents Responses-only state from changing the
/// broadly-compatible Chat Completions implementation.
pub struct OpenAiProvider {
    inner: OpenAiProviderInner,
}

enum OpenAiProviderInner {
    Chat(OpenAiCompatProvider),
    Responses(OpenAiResponsesProvider),
}

impl OpenAiProvider {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        protocol: OpenAiProtocol,
        reasoning_summary: ReasoningSummary,
    ) -> Self {
        let inner = match protocol {
            OpenAiProtocol::ChatCompletions => {
                OpenAiProviderInner::Chat(OpenAiCompatProvider::new(base_url, model, api_key))
            }
            OpenAiProtocol::Responses => {
                OpenAiProviderInner::Responses(OpenAiResponsesProvider::new(
                    base_url,
                    model,
                    api_key,
                    reasoning_summary == ReasoningSummary::Auto,
                ))
            }
        };
        Self { inner }
    }

    pub fn test_connection(&self) -> Result<String, ProviderError> {
        match &self.inner {
            OpenAiProviderInner::Chat(provider) => provider.test_connection(),
            OpenAiProviderInner::Responses(provider) => provider.test_connection(),
        }
    }
}

impl ChatProvider for OpenAiProvider {
    fn chat(
        &self,
        request: &ChatRequest<'_>,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(ProviderStreamEvent<'_>),
    ) -> Result<ChatTurn, ProviderError> {
        match &self.inner {
            OpenAiProviderInner::Chat(provider) => provider.chat(request, cancel, on_event),
            OpenAiProviderInner::Responses(provider) => provider.chat(request, cancel, on_event),
        }
    }
}

//! OpenAI Responses API provider for reasoning models with function tools.
//!
//! Requests are stateless (`store: false`). During one agent prompt, the
//! provider retains only the previous response's output items so encrypted
//! reasoning and function calls can be replayed with the corresponding tool
//! outputs. The desktop creates a fresh provider for every user prompt, and
//! terminal responses clear this in-memory state.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;

use crate::provider::{
    ChatProvider, ChatRequest, ChatTurn, FinishReason, ImagePart, Message, ProviderError,
    ProviderStreamEvent, ToolCall,
};

use super::openai_compat::{retry_delay, retryable_status, sleep_unless_cancelled, truncate};

/// A Responses API transport. One instance is scoped to one agent prompt so
/// replay state cannot leak into persisted conversation history.
pub struct OpenAiResponsesProvider {
    base_url: String,
    model: String,
    api_key: Option<String>,
    reasoning_summary: bool,
    agent: ureq::Agent,
    replay: Mutex<Option<ReplayState>>,
}

#[derive(Clone)]
struct ReplayState {
    /// Messages through the user prompt and persisted text history. Items after
    /// this boundary belong to the current tool loop and are replayed exactly.
    base_message_count: usize,
    /// Number of provider-agnostic messages present before the response that
    /// produced the latest output. The agent appends its assistant turn here.
    request_message_count: usize,
    /// Exact Responses output and function-call-output items since the latest
    /// user message, including encrypted reasoning from every tool round.
    continuation: Vec<serde_json::Value>,
    call_ids: HashSet<String>,
}

#[derive(Debug)]
struct ParsedResponse {
    turn: ChatTurn,
    output: Vec<serde_json::Value>,
}

impl OpenAiResponsesProvider {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        reasoning_summary: bool,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            reasoning_summary,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .build(),
            replay: Mutex::new(None),
        }
    }

    /// The Responses protocol shares the provider's token-free `/models`
    /// liveness endpoint with Chat Completions.
    pub fn test_connection(&self) -> Result<String, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let mut http = self.agent.get(&url);
        if let Some(key) = &self.api_key {
            http = http.set("Authorization", &format!("Bearer {key}"));
        }
        match http.call() {
            Ok(response) => {
                let body = response
                    .into_string()
                    .map_err(|e| ProviderError::Network(format!("reading /models: {e}")))?;
                let parsed: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|e| ProviderError::Protocol(format!("bad /models response: {e}")))?;
                let count = parsed["data"].as_array().map_or(0, Vec::len);
                Ok(match count {
                    0 => "Connected.".to_string(),
                    1 => "Connected · 1 model available.".to_string(),
                    n => format!("Connected · {n} models available."),
                })
            }
            Err(ureq::Error::Status(status, response)) => {
                let message = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable error body>".to_string());
                Err(ProviderError::Provider {
                    status,
                    message: truncate(&message, 200),
                })
            }
            Err(ureq::Error::Transport(error)) => {
                Err(ProviderError::Network(format!("{url}: {error}")))
            }
        }
    }

    fn request_body(&self, request: &ChatRequest<'_>) -> serde_json::Value {
        let instructions = request
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::System { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let replay = self.replay.lock().unwrap().clone();
        let input = match replay.filter(|state| replay_matches(state, request.messages)) {
            Some(state) => replay_input(request.messages, &state),
            None => to_responses_input(request.messages),
        };
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "name": &tool.name,
                    "description": &tool.description,
                    "parameters": &tool.parameters,
                    "strict": false,
                })
            })
            .collect::<Vec<_>>();

        let mut body = serde_json::json!({
            "model": self.model,
            "stream": true,
            "store": false,
            "instructions": instructions,
            "input": input,
            "include": ["reasoning.encrypted_content"],
        });
        if !tools.is_empty() {
            body["tools"] = tools.into();
        }
        if self.reasoning_summary {
            body["reasoning"] = serde_json::json!({ "summary": "auto" });
        }
        body
    }

    fn update_replay(&self, messages: &[Message], turn: &ChatTurn, output: Vec<serde_json::Value>) {
        let mut replay = self.replay.lock().unwrap();
        if turn.finish == FinishReason::ToolCalls && !turn.tool_calls.is_empty() {
            let (base_message_count, mut continuation) = match replay.take() {
                Some(state) if replay_matches(&state, messages) => {
                    let tool_outputs = replay_tool_outputs(messages, &state);
                    let base_message_count = state.base_message_count;
                    let mut continuation = state.continuation;
                    continuation.extend(tool_outputs);
                    (base_message_count, continuation)
                }
                _ => (messages.len(), Vec::new()),
            };
            continuation.extend(output);
            *replay = Some(ReplayState {
                base_message_count,
                request_message_count: messages.len(),
                continuation,
                call_ids: turn.tool_calls.iter().map(|call| call.id.clone()).collect(),
            });
        } else {
            *replay = None;
        }
    }

    fn clear_replay(&self) {
        *self.replay.lock().unwrap() = None;
    }
}

impl ChatProvider for OpenAiResponsesProvider {
    fn chat(
        &self,
        request: &ChatRequest<'_>,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(ProviderStreamEvent<'_>),
    ) -> Result<ChatTurn, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let body = self.request_body(request).to_string();

        // Match Chat Completions: retry only while sending the initial
        // request. Once an SSE reader exists, emitted deltas are never replayed.
        let mut attempt = 0usize;
        let response = loop {
            if cancel.load(Ordering::Relaxed) {
                self.clear_replay();
                return Err(ProviderError::Cancelled);
            }
            let mut http = self
                .agent
                .post(&url)
                .set("Content-Type", "application/json")
                .set("Accept", "text/event-stream");
            if let Some(key) = &self.api_key {
                http = http.set("Authorization", &format!("Bearer {key}"));
            }
            match http.send_string(&body) {
                Ok(response) => break response,
                Err(ureq::Error::Status(status, response)) => {
                    if retryable_status(status) {
                        match retry_delay(attempt) {
                            Some(delay) if sleep_unless_cancelled(delay, cancel) => {
                                attempt += 1;
                                continue;
                            }
                            Some(_) => {
                                self.clear_replay();
                                return Err(ProviderError::Cancelled);
                            }
                            None => {}
                        }
                    }
                    let message = response
                        .into_string()
                        .unwrap_or_else(|_| "<unreadable error body>".to_string());
                    self.clear_replay();
                    return Err(ProviderError::Provider {
                        status,
                        message: truncate(&message, 500),
                    });
                }
                Err(ureq::Error::Transport(error)) => match retry_delay(attempt) {
                    Some(delay) if sleep_unless_cancelled(delay, cancel) => attempt += 1,
                    Some(_) => {
                        self.clear_replay();
                        return Err(ProviderError::Cancelled);
                    }
                    None => {
                        self.clear_replay();
                        return Err(ProviderError::Network(format!("{url}: {error}")));
                    }
                },
            }
        };

        match consume_responses_sse(response.into_reader(), cancel, on_event) {
            Ok(parsed) => {
                self.update_replay(request.messages, &parsed.turn, parsed.output);
                Ok(parsed.turn)
            }
            Err(error) => {
                self.clear_replay();
                Err(error)
            }
        }
    }
}

fn replay_matches(state: &ReplayState, messages: &[Message]) -> bool {
    let Some(Message::Assistant { tool_calls, .. }) = messages.get(state.request_message_count)
    else {
        return false;
    };
    let assistant_ids = tool_calls
        .iter()
        .map(|call| call.id.as_str())
        .collect::<HashSet<_>>();
    let result_ids = messages[state.request_message_count + 1..]
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    assistant_ids.len() == state.call_ids.len()
        && state
            .call_ids
            .iter()
            .all(|id| assistant_ids.contains(id.as_str()))
        && state
            .call_ids
            .iter()
            .all(|id| result_ids.contains(id.as_str()))
}

fn replay_input(messages: &[Message], state: &ReplayState) -> Vec<serde_json::Value> {
    let mut input = to_responses_input(&messages[..state.base_message_count]);
    input.extend(state.continuation.iter().cloned());
    input.extend(replay_tool_outputs(messages, state));
    input
}

fn replay_tool_outputs(messages: &[Message], state: &ReplayState) -> Vec<serde_json::Value> {
    messages[state.request_message_count + 1..]
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { call_id, .. } if state.call_ids.contains(call_id) => {
                Some(tool_result_input(message))
            }
            _ => None,
        })
        .collect()
}

fn to_responses_input(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut input = Vec::new();
    for message in messages {
        match message {
            Message::System { .. } => {}
            Message::User { content, images } => {
                input.push(user_input_message(content, images));
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                if !content.is_empty() {
                    input.push(assistant_output_message(content));
                }
                input.extend(tool_calls.iter().map(|call| {
                    serde_json::json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    })
                }));
            }
            Message::ToolResult { .. } => input.push(tool_result_input(message)),
        }
    }
    input
}

fn user_input_message(content: &str, images: &[ImagePart]) -> serde_json::Value {
    let mut parts = vec![serde_json::json!({
        "type": "input_text",
        "text": content,
    })];
    parts.extend(images.iter().map(input_image_part));
    serde_json::json!({
        "role": "user",
        "content": parts,
    })
}

fn assistant_output_message(content: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": content,
        }],
    })
}

fn tool_result_input(message: &Message) -> serde_json::Value {
    let Message::ToolResult {
        call_id,
        content,
        images,
    } = message
    else {
        unreachable!("tool_result_input requires a tool result");
    };
    if images.is_empty() {
        return serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": content,
        });
    }

    let labels = images
        .iter()
        .map(|image| image.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut output = vec![serde_json::json!({
        "type": "input_text",
        "text": format!("{content}\n[attachments: {labels}]"),
    })];
    output.extend(images.iter().map(input_image_part));
    serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output,
    })
}

fn input_image_part(image: &ImagePart) -> serde_json::Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(image.data.as_slice());
    serde_json::json!({
        "type": "input_image",
        "image_url": format!("data:{};base64,{encoded}", image.media_type),
        "detail": "auto",
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        item: serde_json::Value,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { output_index: usize, delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        output_index: usize,
        arguments: String,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        item: serde_json::Value,
    },
    #[serde(rename = "response.completed")]
    Completed { response: ResponseEnvelope },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: ResponseEnvelope },
    #[serde(rename = "response.failed")]
    Failed { response: ResponseEnvelope },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct ResponseEnvelope {
    #[serde(default)]
    output: Vec<serde_json::Value>,
    #[serde(default)]
    error: Option<ResponseApiError>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct ResponseApiError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Default, Deserialize)]
struct IncompleteDetails {
    #[serde(default)]
    reason: String,
}

enum Terminal {
    Completed(ResponseEnvelope),
    Incomplete(ResponseEnvelope),
}

/// Parse typed Responses events while preserving separate answer and provider
/// reasoning-summary channels.
fn consume_responses_sse(
    reader: impl Read,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(ProviderStreamEvent<'_>),
) -> Result<ParsedResponse, ProviderError> {
    let mut streamed_text = String::new();
    let mut streamed_reasoning = String::new();
    let mut output_items = BTreeMap::<usize, serde_json::Value>::new();
    let mut terminal = None;

    for line in BufReader::new(reader).lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ProviderError::Cancelled);
        }
        let line = line.map_err(|error| {
            ProviderError::Network(format!("Responses stream read failed: {error}"))
        })?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let event: ResponsesEvent = serde_json::from_str(data).map_err(|error| {
            ProviderError::Protocol(format!("bad Responses SSE event: {error}: {data}"))
        })?;
        match event {
            ResponsesEvent::OutputTextDelta { delta } => {
                if !delta.is_empty() {
                    streamed_text.push_str(&delta);
                    on_event(ProviderStreamEvent::TextDelta(&delta));
                }
            }
            ResponsesEvent::ReasoningSummaryTextDelta { delta } => {
                if !delta.is_empty() {
                    streamed_reasoning.push_str(&delta);
                    on_event(ProviderStreamEvent::ReasoningSummaryDelta(&delta));
                }
            }
            ResponsesEvent::OutputItemAdded { output_index, item }
            | ResponsesEvent::OutputItemDone { output_index, item } => {
                output_items.insert(output_index, item);
            }
            ResponsesEvent::FunctionCallArgumentsDelta {
                output_index,
                delta,
            } => append_function_arguments(&mut output_items, output_index, &delta),
            ResponsesEvent::FunctionCallArgumentsDone {
                output_index,
                arguments,
            } => set_function_arguments(&mut output_items, output_index, arguments),
            ResponsesEvent::Completed { response } => {
                terminal = Some(Terminal::Completed(response));
                break;
            }
            ResponsesEvent::Incomplete { response } => {
                terminal = Some(Terminal::Incomplete(response));
                break;
            }
            ResponsesEvent::Failed { response } => {
                return Err(ProviderError::Protocol(format!(
                    "Responses request failed: {}",
                    response_error_message(&response)
                )));
            }
            ResponsesEvent::Error { message } => {
                return Err(ProviderError::Protocol(format!(
                    "Responses stream error: {message}"
                )));
            }
            ResponsesEvent::Other => {}
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(ProviderError::Cancelled);
    }
    let Some(terminal) = terminal else {
        return Err(ProviderError::Protocol(
            "Responses stream ended without a terminal event".to_string(),
        ));
    };
    let (envelope, finish) = match terminal {
        Terminal::Completed(response) => (response, FinishReason::Stop),
        Terminal::Incomplete(response) => {
            let reason = response
                .incomplete_details
                .as_ref()
                .map(|details| details.reason.as_str())
                .unwrap_or("unknown");
            if !matches!(reason, "max_output_tokens" | "max_tokens") {
                return Err(ProviderError::Protocol(format!(
                    "Responses request was incomplete: {reason}"
                )));
            }
            (response, FinishReason::Length)
        }
    };

    let output = if envelope.output.is_empty() {
        output_items.into_values().collect()
    } else {
        envelope.output
    };
    let final_text = output_text(&output);
    let text = if streamed_text.is_empty() {
        if !final_text.is_empty() {
            on_event(ProviderStreamEvent::TextDelta(&final_text));
        }
        final_text
    } else {
        streamed_text
    };
    let final_reasoning = reasoning_summary(&output);
    let reasoning_summary = if streamed_reasoning.is_empty() {
        if !final_reasoning.is_empty() {
            on_event(ProviderStreamEvent::ReasoningSummaryDelta(&final_reasoning));
        }
        final_reasoning
    } else {
        streamed_reasoning
    };
    let tool_calls = if finish == FinishReason::Stop {
        parse_function_calls(&output)?
    } else {
        Vec::new()
    };
    let finish = if !tool_calls.is_empty() {
        FinishReason::ToolCalls
    } else {
        finish
    };

    Ok(ParsedResponse {
        turn: ChatTurn {
            text,
            reasoning_summary,
            tool_calls,
            finish,
        },
        output,
    })
}

fn append_function_arguments(
    items: &mut BTreeMap<usize, serde_json::Value>,
    output_index: usize,
    delta: &str,
) {
    let item = items.entry(output_index).or_insert_with(|| {
        serde_json::json!({
            "type": "function_call",
            "arguments": "",
        })
    });
    let arguments = item
        .as_object_mut()
        .expect("internally-created output item is an object")
        .entry("arguments")
        .or_insert_with(|| serde_json::Value::String(String::new()));
    let combined = arguments
        .as_str()
        .map(|current| format!("{current}{delta}"))
        .unwrap_or_else(|| delta.to_string());
    *arguments = serde_json::Value::String(combined);
}

fn set_function_arguments(
    items: &mut BTreeMap<usize, serde_json::Value>,
    output_index: usize,
    arguments: String,
) {
    let item = items.entry(output_index).or_insert_with(|| {
        serde_json::json!({
            "type": "function_call",
        })
    });
    item["arguments"] = arguments.into();
}

fn output_text(output: &[serde_json::Value]) -> String {
    output
        .iter()
        .filter(|item| item["type"] == "message")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect()
}

fn reasoning_summary(output: &[serde_json::Value]) -> String {
    output
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .filter_map(|item| item["summary"].as_array())
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect()
}

fn parse_function_calls(output: &[serde_json::Value]) -> Result<Vec<ToolCall>, ProviderError> {
    output
        .iter()
        .filter(|item| item["type"] == "function_call")
        .map(|item| {
            let id = item["call_id"]
                .as_str()
                .or_else(|| item["id"].as_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ProviderError::Protocol(
                        "Responses function call is missing call_id".to_string(),
                    )
                })?;
            let name = item["name"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ProviderError::Protocol(format!(
                        "Responses function call {id} is missing a name"
                    ))
                })?;
            let raw_arguments = item["arguments"].as_str().unwrap_or("");
            let arguments = if raw_arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(raw_arguments).map_err(|error| {
                    ProviderError::Protocol(format!(
                        "Responses function call '{name}' has unparseable arguments: \
                         {error}: {raw_arguments}"
                    ))
                })?
            };
            Ok(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            })
        })
        .collect()
}

fn response_error_message(response: &ResponseEnvelope) -> String {
    response
        .error
        .as_ref()
        .map(|error| error.message.as_str())
        .filter(|message| !message.is_empty())
        .unwrap_or("unknown provider error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ToolSpec;
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};

    fn provider(reasoning_summary: bool) -> OpenAiResponsesProvider {
        OpenAiResponsesProvider::new(
            "https://api.example.test/v1/",
            "gpt-reasoning",
            None,
            reasoning_summary,
        )
    }

    fn run(fixture: &str) -> (ParsedResponse, String, String) {
        let cancel = AtomicBool::new(false);
        let mut text = String::new();
        let mut reasoning = String::new();
        let parsed = consume_responses_sse(fixture.as_bytes(), &cancel, &mut |event| match event {
            ProviderStreamEvent::TextDelta(delta) => text.push_str(delta),
            ProviderStreamEvent::ReasoningSummaryDelta(delta) => reasoning.push_str(delta),
        })
        .expect("fixture parses");
        (parsed, text, reasoning)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 2048];
        let mut expected = None;
        loop {
            let count = stream.read(&mut buffer).expect("read HTTP request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if expected.is_none()
                && let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected = Some(header_end + 4 + content_length);
            }
            if expected.is_some_and(|expected| bytes.len() >= expected) {
                break;
            }
        }
        String::from_utf8(bytes).expect("ASCII HTTP request")
    }

    #[test]
    fn request_maps_instructions_multimodal_input_and_native_tools() {
        let provider = provider(true);
        assert_eq!(provider.base_url, "https://api.example.test/v1");
        let messages = vec![
            Message::system("Fresh project snapshot."),
            Message::User {
                content: "Inspect this frame.".into(),
                images: vec![ImagePart::png(vec![1, 2, 3], "preview")],
            },
        ];
        let tools = vec![ToolSpec {
            name: "trim_clip".into(),
            description: "Trim a clip".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = provider.request_body(&ChatRequest {
            messages: &messages,
            tools: &tools,
        });

        assert_eq!(body["model"], "gpt-reasoning");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "Fresh project snapshot.");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert!(
            body["input"][0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "trim_clip");
        assert_eq!(body["tools"][0]["strict"], false);
        assert!(body["tools"][0].get("function").is_none());
    }

    #[test]
    fn mixed_history_uses_role_specific_content_parts_and_summary_off() {
        let provider = provider(false);
        let messages = [
            Message::system("system"),
            Message::user("Prior question"),
            Message::assistant_text("Prior answer"),
            Message::user("Current question"),
        ];
        let body = provider.request_body(&ChatRequest {
            messages: &messages,
            tools: &[],
        });
        assert!(body.get("reasoning").is_none());
        assert!(body.get("tools").is_none());
        assert_eq!(body["store"], false);
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "Prior question");
        assert_eq!(body["input"][1]["role"], "assistant");
        assert_eq!(body["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(body["input"][1]["content"][0]["text"], "Prior answer");
        assert_eq!(body["input"][2]["role"], "user");
        assert_eq!(body["input"][2]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][2]["content"][0]["text"], "Current question");
    }

    #[test]
    fn encrypted_reasoning_and_parallel_calls_replay_with_native_outputs() {
        let provider = provider(true);
        let output = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "encrypted-private-state",
                "summary": [{"type": "summary_text", "text": "I inspected both clips."}],
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_a",
                "name": "media_asset_strip",
                "arguments": "{\"media_id\":1}",
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_2",
                "call_id": "call_b",
                "name": "describe_project",
                "arguments": "{}",
            }),
        ];
        let turn = ChatTurn {
            text: String::new(),
            reasoning_summary: "I inspected both clips.".into(),
            tool_calls: vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "media_asset_strip".into(),
                    arguments: serde_json::json!({"media_id": 1}),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "describe_project".into(),
                    arguments: serde_json::json!({}),
                },
            ],
            finish: FinishReason::ToolCalls,
        };
        let base_messages = vec![Message::system("system"), Message::user("make a montage")];
        provider.update_replay(&base_messages, &turn, output);

        let mut messages = base_messages;
        messages.extend([
            Message::Assistant {
                content: String::new(),
                tool_calls: turn.tool_calls.clone(),
            },
            Message::ToolResult {
                call_id: "call_a".into(),
                content: "strip ready".into(),
                images: vec![ImagePart::jpeg(vec![9, 8], "source strip")],
            },
            Message::tool_result("call_b", "project state"),
        ]);
        let body = provider.request_body(&ChatRequest {
            messages: &messages,
            tools: &[],
        });
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 6);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "encrypted-private-state");
        assert_eq!(input[2]["call_id"], "call_a");
        assert_eq!(input[3]["call_id"], "call_b");
        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "call_a");
        assert_eq!(input[4]["output"][1]["type"], "input_image");
        assert_eq!(input[5]["call_id"], "call_b");
    }

    #[test]
    fn unrelated_request_does_not_replay_stale_output() {
        let provider = provider(true);
        let turn = ChatTurn {
            text: String::new(),
            reasoning_summary: String::new(),
            tool_calls: vec![ToolCall {
                id: "old_call".into(),
                name: "describe_project".into(),
                arguments: serde_json::json!({}),
            }],
            finish: FinishReason::ToolCalls,
        };
        provider.update_replay(
            &[Message::system("old"), Message::user("old prompt")],
            &turn,
            vec![serde_json::json!({
                "type": "reasoning",
                "encrypted_content": "must-not-leak",
            })],
        );
        let messages = [Message::system("new"), Message::user("new prompt")];
        let body = provider.request_body(&ChatRequest {
            messages: &messages,
            tools: &[],
        });
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert!(!body.to_string().contains("must-not-leak"));
    }

    #[test]
    fn consecutive_tool_rounds_preserve_every_exact_item_since_the_user() {
        let provider = provider(true);
        let mut messages = vec![Message::system("system"), Message::user("edit this")];
        let first_turn = ChatTurn {
            text: String::new(),
            reasoning_summary: "first".into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "describe_project".into(),
                arguments: serde_json::json!({}),
            }],
            finish: FinishReason::ToolCalls,
        };
        provider.update_replay(
            &messages,
            &first_turn,
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    "encrypted_content": "encrypted-round-1",
                    "summary": [],
                }),
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "describe_project",
                    "arguments": "{}",
                }),
            ],
        );
        messages.push(Message::Assistant {
            content: String::new(),
            tool_calls: first_turn.tool_calls,
        });
        messages.push(Message::tool_result("call_1", "round one result"));

        let second_turn = ChatTurn {
            text: String::new(),
            reasoning_summary: "second".into(),
            tool_calls: vec![ToolCall {
                id: "call_2".into(),
                name: "remove_clip".into(),
                arguments: serde_json::json!({"clip": 7}),
            }],
            finish: FinishReason::ToolCalls,
        };
        provider.update_replay(
            &messages,
            &second_turn,
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    "encrypted_content": "encrypted-round-2",
                    "summary": [],
                }),
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call_2",
                    "name": "remove_clip",
                    "arguments": "{\"clip\":7}",
                }),
            ],
        );
        messages.push(Message::Assistant {
            content: String::new(),
            tool_calls: second_turn.tool_calls,
        });
        messages.push(Message::tool_result("call_2", "round two result"));

        let body = provider.request_body(&ChatRequest {
            messages: &messages,
            tools: &[],
        });
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 7);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["encrypted_content"], "encrypted-round-1");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[3]["output"], "round one result");
        assert_eq!(input[4]["encrypted_content"], "encrypted-round-2");
        assert_eq!(input[5]["call_id"], "call_2");
        assert_eq!(input[6]["call_id"], "call_2");
        assert_eq!(input[6]["output"], "round two result");
    }

    #[test]
    fn text_and_reasoning_summary_streams_stay_separate() {
        let fixture = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"I checked \"}\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"the cuts.\"}\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\".\"}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"reasoning\",\"encrypted_content\":\"enc\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"I checked the cuts.\"}]},",
            "{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done.\"}]}",
            "]}}\n",
        );
        let (parsed, streamed, reasoning) = run(fixture);
        assert_eq!(streamed, "Done.");
        assert_eq!(reasoning, "I checked the cuts.");
        assert_eq!(parsed.turn.text, "Done.");
        assert_eq!(parsed.turn.reasoning_summary, "I checked the cuts.");
        assert_eq!(parsed.turn.finish, FinishReason::Stop);
        assert!(parsed.turn.tool_calls.is_empty());
        assert_eq!(parsed.output[0]["encrypted_content"], "enc");
    }

    #[test]
    fn completed_output_parses_parallel_function_calls() {
        let fixture = concat!(
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"clip\\\":\"}\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"clip\\\":1}\"}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"function_call\",\"id\":\"fc_a\",\"call_id\":\"call_a\",\"name\":\"remove_clip\",\"arguments\":\"{\\\"clip\\\":1}\"},",
            "{\"type\":\"function_call\",\"id\":\"fc_b\",\"call_id\":\"call_b\",\"name\":\"remove_clip\",\"arguments\":\"{\\\"clip\\\":2}\"}",
            "]}}\n",
        );
        let (parsed, streamed, reasoning) = run(fixture);
        assert!(streamed.is_empty());
        assert!(reasoning.is_empty());
        assert_eq!(parsed.turn.finish, FinishReason::ToolCalls);
        assert_eq!(parsed.turn.tool_calls.len(), 2);
        assert_eq!(parsed.turn.tool_calls[0].id, "call_a");
        assert_eq!(parsed.turn.tool_calls[0].arguments["clip"], 1);
        assert_eq!(parsed.turn.tool_calls[1].id, "call_b");
        assert_eq!(parsed.turn.tool_calls[1].arguments["clip"], 2);
    }

    #[test]
    fn output_item_events_are_a_terminal_output_fallback() {
        let fixture = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"remove_clip\",\"arguments\":\"\"}}\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"clip\\\":3}\"}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n",
        );
        let (parsed, _, _) = run(fixture);
        assert_eq!(parsed.turn.tool_calls.len(), 1);
        assert_eq!(parsed.turn.tool_calls[0].arguments["clip"], 3);
    }

    #[test]
    fn terminal_display_events_are_forwarded_when_deltas_are_absent() {
        let fixture = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Checked the result.\"}]},",
            "{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Complete answer\"}]}",
            "]}}\n",
        );
        let (parsed, streamed, reasoning) = run(fixture);
        assert_eq!(streamed, "Complete answer");
        assert_eq!(reasoning, "Checked the result.");
        assert_eq!(parsed.turn.text, streamed);
        assert_eq!(parsed.turn.reasoning_summary, reasoning);
    }

    #[test]
    fn malformed_events_arguments_and_missing_terminal_are_errors() {
        let cancel = AtomicBool::new(false);
        let malformed =
            consume_responses_sse("data: {not json}\n".as_bytes(), &cancel, &mut |_| {})
                .unwrap_err();
        assert!(matches!(malformed, ProviderError::Protocol(_)));

        let bad_arguments = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"trim_clip\",\"arguments\":\"{oops\"}",
            "]}}\n",
        );
        let error =
            consume_responses_sse(bad_arguments.as_bytes(), &cancel, &mut |_| {}).unwrap_err();
        match error {
            ProviderError::Protocol(message) => assert!(message.contains("trim_clip"), "{message}"),
            other => panic!("expected protocol error, got {other:?}"),
        }

        let no_terminal = consume_responses_sse(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n".as_bytes(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        match no_terminal {
            ProviderError::Protocol(message) => {
                assert!(message.contains("terminal event"), "{message}");
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn cancellation_and_incomplete_responses_are_distinct() {
        let cancel = AtomicBool::new(true);
        let cancelled = consume_responses_sse(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n".as_bytes(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(cancelled, ProviderError::Cancelled));

        let fixture = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Partial\"}\n",
            "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[]}}\n",
        );
        let (parsed, streamed, reasoning) = run(fixture);
        assert_eq!(streamed, "Partial");
        assert!(reasoning.is_empty());
        assert_eq!(parsed.turn.finish, FinishReason::Length);
        assert!(parsed.turn.tool_calls.is_empty());
    }

    #[test]
    fn failed_and_unknown_incomplete_responses_report_provider_details() {
        let cancel = AtomicBool::new(false);
        let stream_error = consume_responses_sse(
            "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"stream broke\",\"param\":null}\n"
                .as_bytes(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        match stream_error {
            ProviderError::Protocol(message) => {
                assert!(message.contains("stream broke"), "{message}");
            }
            other => panic!("expected protocol error, got {other:?}"),
        }

        let failed = consume_responses_sse(
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"model unavailable\"}}}\n"
                .as_bytes(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        match failed {
            ProviderError::Protocol(message) => {
                assert!(message.contains("model unavailable"), "{message}");
            }
            other => panic!("expected protocol error, got {other:?}"),
        }

        let incomplete = consume_responses_sse(
            "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"content_filter\"},\"output\":[]}}\n"
                .as_bytes(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        match incomplete {
            ProviderError::Protocol(message) => {
                assert!(message.contains("content_filter"), "{message}");
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn initial_transient_status_retries_but_preflight_cancellation_does_not_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("provider connects");
                requests.push(read_http_request(&mut stream));
                let (status, body) = if attempt == 0 {
                    ("500 Internal Server Error", "temporary")
                } else {
                    (
                        "200 OK",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\
                         data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n",
                    )
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                stream.flush().unwrap();
            }
            requests
        });

        let provider = OpenAiResponsesProvider::new(
            &format!("http://{address}/v1"),
            "reasoning-model",
            None,
            true,
        );
        let messages = [Message::system("system"), Message::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: &[],
        };
        let cancel = AtomicBool::new(false);
        let mut streamed = String::new();
        let turn = provider
            .chat(&request, &cancel, &mut |event| match event {
                ProviderStreamEvent::TextDelta(delta) => streamed.push_str(delta),
                ProviderStreamEvent::ReasoningSummaryDelta(_) => {}
            })
            .expect("second initial request succeeds");
        assert_eq!(turn.text, "ok");
        assert_eq!(streamed, "ok");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.starts_with("POST /v1/responses HTTP/1.1")),
            "{requests:#?}"
        );

        let cancelled = AtomicBool::new(true);
        let error = provider
            .chat(&request, &cancelled, &mut |_| {})
            .unwrap_err();
        assert!(matches!(error, ProviderError::Cancelled));
    }
}

use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{
    header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::{
    endpoint::{validate_openai_url, CHAT_COMPLETIONS_URL},
    ApiKey, CancellationToken, ChatError, TransportError, CHAT_MODEL,
};

const MAX_CHAT_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_SSE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_COMPLETION_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOL_CALLS: usize = 64;
const MAX_TOOL_CALL_ID_BYTES: usize = 512;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    Developer,
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AssistantToolCall>>,
}

impl ChatMessage {
    pub fn text(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Value::String(content.into()),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: Value::String(content.into()),
            name: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatTool {
    #[serde(rename = "type")]
    kind: FunctionToolKind,
    pub function: FunctionDefinition,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FunctionToolKind {
    Function,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantToolCall {
    pub id: String,
    #[serde(rename = "type")]
    kind: FunctionToolKind,
    pub function: AssistantFunctionCall,
}

impl AssistantToolCall {
    pub fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: FunctionToolKind::Function,
            function: AssistantFunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

impl ChatTool {
    pub fn function(function: FunctionDefinition) -> Self {
        Self {
            kind: FunctionToolKind::Function,
            function,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ChatTool>,
    pub max_completion_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            max_completion_tokens: None,
            reasoning_effort: None,
        }
    }

    pub fn encoded(&self) -> Result<Vec<u8>, ChatError> {
        if !self.tools.is_empty() && self.reasoning_effort != Some(ReasoningEffort::None) {
            return Err(ChatError::UnsupportedToolReasoning);
        }
        let body = serde_json::to_vec(&WireChatRequest {
            model: CHAT_MODEL,
            messages: &self.messages,
            tools: (!self.tools.is_empty()).then_some(self.tools.as_slice()),
            max_completion_tokens: self.max_completion_tokens,
            reasoning_effort: self.reasoning_effort,
            stream: true,
            store: false,
            stream_options: StreamOptions {
                include_usage: true,
            },
        })
        .map_err(|_| ChatError::Encode)?;
        if body.len() > MAX_CHAT_REQUEST_BYTES {
            return Err(ChatError::Encode);
        }
        Ok(body)
    }
}

#[derive(Serialize)]
struct WireChatRequest<'a> {
    model: &'static str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ChatTool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    stream: bool,
    store: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl FunctionToolCall {
    pub fn arguments_json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

impl ChatMessage {
    pub fn assistant_tool_calls(
        content: impl Into<String>,
        tool_calls: &[FunctionToolCall],
    ) -> Self {
        let content = content.into();
        Self {
            role: ChatRole::Assistant,
            content: if content.is_empty() {
                Value::Null
            } else {
                Value::String(content)
            },
            name: None,
            tool_call_id: None,
            tool_calls: Some(
                tool_calls
                    .iter()
                    .map(|call| {
                        AssistantToolCall::function(
                            call.id.clone(),
                            call.name.clone(),
                            call.arguments.clone(),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCompletion {
    pub text: String,
    pub tool_calls: Vec<FunctionToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatStreamEvent {
    ContentDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Completed(ChatCompletion),
}

#[async_trait]
pub trait ChatTransport: Send + Sync {
    async fn open(&self, key: &ApiKey, body: Vec<u8>) -> Result<ByteStream, TransportError>;
}

#[derive(Clone, Debug)]
pub struct HttpsChatTransport {
    client: reqwest::Client,
}

impl HttpsChatTransport {
    pub fn new() -> Result<Self, TransportError> {
        debug_assert!(validate_openai_url(CHAT_COMPLETIONS_URL, false));
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            // Avoid environment-configured HTTP proxies: runtime traffic must
            // terminate directly at api.openai.com.
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .user_agent("woof/0.1.0")
            .build()
            .map_err(|_| TransportError::PermanentNetwork)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl ChatTransport for HttpsChatTransport {
    async fn open(&self, key: &ApiKey, body: Vec<u8>) -> Result<ByteStream, TransportError> {
        let bearer = Zeroizing::new(format!("Bearer {}", key.expose()));
        let authorization = HeaderValue::from_bytes(bearer.as_bytes())
            .map_err(|_| TransportError::PermanentNetwork)?;
        let response = self
            .client
            .post(CHAT_COMPLETIONS_URL)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(TransportError::Http {
                status,
                retry_after,
            });
        }

        let stream = response
            .bytes_stream()
            .map(|result| result.map_err(map_reqwest_error));
        Ok(Box::pin(stream))
    }
}

fn map_reqwest_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() {
        TransportError::TransientNetwork
    } else {
        TransportError::PermanentNetwork
    }
}

#[derive(Clone, Debug)]
pub struct ChatClient<T = HttpsChatTransport> {
    transport: T,
    max_attempts: usize,
    retry_base: Duration,
    retry_cap: Duration,
}

impl ChatClient<HttpsChatTransport> {
    pub fn openai() -> Result<Self, TransportError> {
        Ok(Self::with_transport(HttpsChatTransport::new()?))
    }
}

impl<T> ChatClient<T>
where
    T: ChatTransport,
{
    pub fn with_transport(transport: T) -> Self {
        Self {
            transport,
            max_attempts: 3,
            retry_base: Duration::from_millis(250),
            retry_cap: Duration::from_secs(8),
        }
    }

    pub fn with_retry_timing(mut self, base: Duration, cap: Duration) -> Self {
        self.retry_base = base;
        self.retry_cap = cap.max(base);
        self
    }

    pub async fn stream_chat<F>(
        &self,
        key: &ApiKey,
        request: &ChatRequest,
        cancellation: &CancellationToken,
        mut callback: F,
    ) -> Result<ChatCompletion, ChatError>
    where
        F: FnMut(ChatStreamEvent) + Send,
    {
        let body = request.encoded()?;

        'attempts: for attempt in 0..self.max_attempts {
            if cancellation.is_cancelled() {
                return Err(ChatError::Cancelled);
            }

            let opened = tokio::select! {
                _ = cancellation.cancelled() => return Err(ChatError::Cancelled),
                opened = self.transport.open(key, body.clone()) => opened,
            };
            let mut stream = match opened {
                Ok(stream) => stream,
                Err(error) if error.retryable() && attempt + 1 < self.max_attempts => {
                    let delay = self.retry_delay(attempt, error.retry_after());
                    wait_or_cancel(delay, cancellation).await?;
                    continue;
                }
                Err(error) => return Err(ChatError::Transport(error)),
            };

            let mut decoder = SseDecoder::default();
            let mut assembly = CompletionAssembly::default();
            let mut emitted = false;

            loop {
                let item = tokio::select! {
                    _ = cancellation.cancelled() => return Err(ChatError::Cancelled),
                    item = stream.next() => item,
                };

                let Some(item) = item else {
                    if !emitted && attempt + 1 < self.max_attempts {
                        wait_or_cancel(self.retry_delay(attempt, None), cancellation).await?;
                        continue 'attempts;
                    }
                    return Err(ChatError::UnexpectedEnd);
                };

                let bytes = match item {
                    Ok(bytes) => bytes,
                    Err(error)
                        if error.retryable() && !emitted && attempt + 1 < self.max_attempts =>
                    {
                        wait_or_cancel(
                            self.retry_delay(attempt, error.retry_after()),
                            cancellation,
                        )
                        .await?;
                        continue 'attempts;
                    }
                    Err(error) => return Err(ChatError::Transport(error)),
                };

                for data in decoder.push(&bytes)? {
                    if data.trim() == "[DONE]" {
                        let completion = assembly.finish()?;
                        callback(ChatStreamEvent::Completed(completion.clone()));
                        return Ok(completion);
                    }
                    let chunk: WireStreamChunk =
                        serde_json::from_str(&data).map_err(|_| ChatError::InvalidStream)?;
                    assembly.apply(chunk, &mut callback, &mut emitted)?;
                }
            }
        }

        Err(ChatError::UnexpectedEnd)
    }

    /// Runs the bounded Chat Completions function-tool loop.
    ///
    /// Tool calls and their results are appended using OpenAI's assistant/tool
    /// message wire format. An executor error is returned to the model as a
    /// structured tool result so one failed local lookup does not discard the
    /// rest of the conversation.
    pub async fn stream_chat_with_tools<E, Fut, F>(
        &self,
        key: &ApiKey,
        request: &ChatRequest,
        cancellation: &CancellationToken,
        mut execute: E,
        mut callback: F,
    ) -> Result<ChatCompletion, ChatError>
    where
        E: FnMut(FunctionToolCall) -> Fut + Send,
        Fut: Future<Output = Result<Value, String>> + Send,
        F: FnMut(ChatStreamEvent) + Send,
    {
        const MAX_TOOL_ROUNDS: usize = 4;

        let mut request = request.clone();
        for round in 0..=MAX_TOOL_ROUNDS {
            let completion = self
                .stream_chat(key, &request, cancellation, &mut callback)
                .await?;
            if completion.tool_calls.is_empty() {
                return Ok(completion);
            }
            if round == MAX_TOOL_ROUNDS {
                return Err(ChatError::ToolRoundsExceeded);
            }

            request.messages.push(ChatMessage::assistant_tool_calls(
                completion.text.clone(),
                &completion.tool_calls,
            ));
            for call in completion.tool_calls {
                let call_id = call.id.clone();
                let outcome = tokio::select! {
                    _ = cancellation.cancelled() => return Err(ChatError::Cancelled),
                    outcome = execute(call) => outcome,
                };
                let result = match outcome {
                    Ok(value) => value,
                    Err(message) => serde_json::json!({"error": message}),
                };
                request.messages.push(ChatMessage::tool_result(
                    call_id,
                    serde_json::to_string(&result).map_err(|_| ChatError::Encode)?,
                ));
            }
        }

        Err(ChatError::ToolRoundsExceeded)
    }

    fn retry_delay(&self, attempt: usize, retry_after: Option<Duration>) -> Duration {
        let exponent = attempt.min(31) as u32;
        let calculated = self
            .retry_base
            .saturating_mul(1_u32 << exponent)
            .min(self.retry_cap);
        retry_after.unwrap_or(calculated).min(self.retry_cap)
    }
}

async fn wait_or_cancel(
    duration: Duration,
    cancellation: &CancellationToken,
) -> Result<(), ChatError> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(ChatError::Cancelled),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, ChatError> {
        if chunk.len() > MAX_SSE_BUFFER_BYTES.saturating_sub(self.buffer.len()) {
            return Err(ChatError::InvalidStream);
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((position, delimiter_length)) = event_boundary(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..position).collect();
            self.buffer.drain(..delimiter_length);
            let block = std::str::from_utf8(&block).map_err(|_| ChatError::InvalidStream)?;
            let data: Vec<&str> = block
                .lines()
                .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
                .map(str::trim_start)
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        Ok(events)
    }
}

fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let unix = buffer.windows(2).position(|window| window == b"\n\n");
    let windows = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (unix, windows) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(position), None) => Some((position, 2)),
        (None, Some(position)) => Some((position, 4)),
        (None, None) => None,
    }
}

#[derive(Debug, Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct WireChoice {
    index: usize,
    delta: WireDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct WireToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<WireFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct WireFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct ToolCallAssembly {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct CompletionAssembly {
    text: String,
    tool_calls: BTreeMap<usize, ToolCallAssembly>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}

impl CompletionAssembly {
    fn apply<F>(
        &mut self,
        chunk: WireStreamChunk,
        callback: &mut F,
        emitted: &mut bool,
    ) -> Result<(), ChatError>
    where
        F: FnMut(ChatStreamEvent),
    {
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                continue;
            }
            if let Some(content) = choice.delta.content {
                if !content.is_empty() {
                    checked_push_str(&mut self.text, &content, MAX_COMPLETION_TEXT_BYTES)?;
                    *emitted = true;
                    callback(ChatStreamEvent::ContentDelta(content));
                }
            }
            for delta in choice.delta.tool_calls {
                if !self.tool_calls.contains_key(&delta.index)
                    && self.tool_calls.len() >= MAX_TOOL_CALLS
                {
                    return Err(ChatError::InvalidStream);
                }
                let function = delta.function.unwrap_or(WireFunctionDelta {
                    name: None,
                    arguments: None,
                });
                let assembly = self.tool_calls.entry(delta.index).or_default();
                if let Some(id) = delta.id.as_deref() {
                    if assembly.id != id {
                        checked_push_str(&mut assembly.id, id, MAX_TOOL_CALL_ID_BYTES)?;
                    }
                }
                if let Some(name) = function.name.as_deref() {
                    checked_push_str(&mut assembly.name, name, MAX_TOOL_NAME_BYTES)?;
                }
                if let Some(arguments) = function.arguments.as_deref() {
                    checked_push_str(&mut assembly.arguments, arguments, MAX_TOOL_ARGUMENT_BYTES)?;
                }
                *emitted = true;
                callback(ChatStreamEvent::ToolCallDelta {
                    index: delta.index,
                    id: delta.id,
                    name: function.name,
                    arguments: function.arguments,
                });
            }
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<ChatCompletion, ChatError> {
        let mut tool_calls = Vec::with_capacity(self.tool_calls.len());
        for (_, value) in self.tool_calls {
            if value.id.is_empty() || value.name.is_empty() {
                return Err(ChatError::InvalidStream);
            }
            if serde_json::from_str::<Value>(&value.arguments).is_err() {
                return Err(ChatError::InvalidStream);
            }
            tool_calls.push(FunctionToolCall {
                id: value.id,
                name: value.name,
                arguments: value.arguments,
            });
        }
        Ok(ChatCompletion {
            text: self.text,
            tool_calls,
            finish_reason: self.finish_reason,
            usage: self.usage,
        })
    }
}

fn checked_push_str(target: &mut String, value: &str, limit: usize) -> Result<(), ChatError> {
    if value.len() > limit.saturating_sub(target.len()) {
        return Err(ChatError::InvalidStream);
    }
    target.push_str(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use futures_util::stream;
    use serde_json::json;

    use super::*;

    type MockOutcome = Result<Vec<Vec<u8>>, TransportError>;

    #[derive(Clone, Default)]
    struct MockTransport {
        outcomes: Arc<Mutex<VecDeque<MockOutcome>>>,
        requests: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[async_trait]
    impl ChatTransport for MockTransport {
        async fn open(&self, _key: &ApiKey, body: Vec<u8>) -> Result<ByteStream, TransportError> {
            self.requests.lock().unwrap().push(body);
            let outcome = self.outcomes.lock().unwrap().pop_front().unwrap();
            outcome.map(|chunks| {
                Box::pin(stream::iter(
                    chunks.into_iter().map(|value| Ok(Bytes::from(value))),
                )) as ByteStream
            })
        }
    }

    fn request() -> ChatRequest {
        ChatRequest::new(vec![ChatMessage::text(ChatRole::User, "hello")])
    }

    #[test]
    fn payload_is_pinned_and_never_stored() {
        let value: Value = serde_json::from_slice(&request().encoded().unwrap()).unwrap();
        assert_eq!(CHAT_MODEL, "gpt-5.6-terra");
        assert_eq!(value["model"], "gpt-5.6-terra");
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn chat_completions_function_tools_require_none_reasoning() {
        let mut request = request();
        request.tools = vec![ChatTool::function(FunctionDefinition {
            name: "search_memory".into(),
            description: "Search local memory".into(),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
            strict: Some(true),
        })];

        request.reasoning_effort = Some(ReasoningEffort::Low);
        assert!(matches!(
            request.encoded(),
            Err(ChatError::UnsupportedToolReasoning)
        ));

        request.reasoning_effort = Some(ReasoningEffort::None);
        let value: Value = serde_json::from_slice(&request.encoded().unwrap()).unwrap();
        assert_eq!(value["reasoning_effort"], "none");
        assert_eq!(value["tools"][0]["function"]["name"], "search_memory");
    }

    #[test]
    fn serializes_assistant_tool_calls_and_tool_results_for_followup_turns() {
        let call = FunctionToolCall {
            id: "call_memory".into(),
            name: "search_memory".into(),
            arguments: r#"{"query":"boxer"}"#.into(),
        };
        let request = ChatRequest::new(vec![
            ChatMessage::assistant_tool_calls("", std::slice::from_ref(&call)),
            ChatMessage::tool_result(&call.id, r#"{"results":[]}"#),
        ]);
        let value: Value = serde_json::from_slice(&request.encoded().unwrap()).unwrap();

        assert_eq!(value["messages"][0]["role"], "assistant");
        assert!(value["messages"][0]["content"].is_null());
        assert_eq!(
            value["messages"][0]["tool_calls"][0],
            json!({
                "id": "call_memory",
                "type": "function",
                "function": {
                    "name": "search_memory",
                    "arguments": "{\"query\":\"boxer\"}"
                }
            })
        );
        assert_eq!(value["messages"][1]["role"], "tool");
        assert_eq!(value["messages"][1]["tool_call_id"], "call_memory");
        assert_eq!(value["messages"][1]["content"], r#"{"results":[]}"#);
        assert!(value["messages"][1].get("tool_calls").is_none());
    }

    #[tokio::test]
    async fn assembles_split_text_and_tool_deltas() {
        let data = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_\",\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"there\",\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"memory\",\"arguments\":\"\\\"dogs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );
        let chunks = vec![
            data.as_bytes()[..17].to_vec(),
            data.as_bytes()[17..93].to_vec(),
            data.as_bytes()[93..].to_vec(),
        ];
        let transport = MockTransport::default();
        transport.outcomes.lock().unwrap().push_back(Ok(chunks));
        let client =
            ChatClient::with_transport(transport).with_retry_timing(Duration::ZERO, Duration::ZERO);
        let mut events = Vec::new();
        let result = client
            .stream_chat(
                &ApiKey::new("sk-test").unwrap(),
                &request(),
                &CancellationToken::new(),
                |event| events.push(event),
            )
            .await
            .unwrap();

        assert_eq!(result.text, "Hi there");
        assert_eq!(result.tool_calls[0].name, "search_memory");
        assert_eq!(result.tool_calls[0].arguments, "{\"q\":\"dogs\"}");
        assert_eq!(result.usage.unwrap().total_tokens, 15);
        assert!(matches!(events.last(), Some(ChatStreamEvent::Completed(_))));
    }

    #[tokio::test]
    async fn executes_tools_and_serializes_the_followup_turn() {
        let first = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_memory\",\"arguments\":\"{\\\"query\\\":\\\"boxer\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let second = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Found it.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let transport = MockTransport::default();
        transport.outcomes.lock().unwrap().extend([
            Ok(vec![first.as_bytes().to_vec()]),
            Ok(vec![second.as_bytes().to_vec()]),
        ]);
        let client = ChatClient::with_transport(transport.clone())
            .with_retry_timing(Duration::ZERO, Duration::ZERO);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed_calls = Arc::clone(&calls);

        let completion = client
            .stream_chat_with_tools(
                &ApiKey::new("sk-test").unwrap(),
                &request(),
                &CancellationToken::new(),
                move |call| {
                    observed_calls.lock().unwrap().push(call.clone());
                    async move { Ok(json!({"results": [{"id": "snapshot-1"}]})) }
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(completion.text, "Found it.");
        assert_eq!(calls.lock().unwrap()[0].name, "search_memory");
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let followup: Value = serde_json::from_slice(&requests[1]).unwrap();
        assert_eq!(followup["messages"][1]["role"], "assistant");
        assert_eq!(
            followup["messages"][1]["tool_calls"][0]["function"]["name"],
            "search_memory"
        );
        assert_eq!(followup["messages"][2]["role"], "tool");
        assert_eq!(followup["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(
            followup["messages"][2]["content"],
            r#"{"results":[{"id":"snapshot-1"}]}"#
        );
    }

    #[tokio::test]
    async fn retries_429_and_5xx_at_most_three_attempts() {
        let transport = MockTransport::default();
        transport.outcomes.lock().unwrap().extend([
            Err(TransportError::Http {
                status: 429,
                retry_after: None,
            }),
            Err(TransportError::Http {
                status: 503,
                retry_after: None,
            }),
            Ok(vec![b"data: [DONE]\n\n".to_vec()]),
        ]);
        let client = ChatClient::with_transport(transport.clone())
            .with_retry_timing(Duration::ZERO, Duration::ZERO);
        client
            .stream_chat(
                &ApiKey::new("sk-test").unwrap(),
                &request(),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(transport.requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn does_not_retry_authentication_or_schema_statuses() {
        for status in [400, 401, 403, 404, 408, 422] {
            let transport = MockTransport::default();
            transport
                .outcomes
                .lock()
                .unwrap()
                .push_back(Err(TransportError::Http {
                    status,
                    retry_after: None,
                }));
            let client = ChatClient::with_transport(transport.clone())
                .with_retry_timing(Duration::ZERO, Duration::ZERO);
            let result = client
                .stream_chat(
                    &ApiKey::new("sk-test").unwrap(),
                    &request(),
                    &CancellationToken::new(),
                    |_| {},
                )
                .await;
            assert!(result.is_err());
            assert_eq!(
                transport.requests.lock().unwrap().len(),
                1,
                "status {status}"
            );
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_wait() {
        let transport = MockTransport::default();
        transport
            .outcomes
            .lock()
            .unwrap()
            .push_back(Err(TransportError::TransientNetwork));
        let client = ChatClient::with_transport(transport)
            .with_retry_timing(Duration::from_secs(5), Duration::from_secs(5));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = client
            .stream_chat(
                &ApiKey::new("sk-test").unwrap(),
                &request(),
                &cancellation,
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(ChatError::Cancelled)));
    }

    #[derive(Clone)]
    struct PendingTransport;

    #[async_trait]
    impl ChatTransport for PendingTransport {
        async fn open(&self, _key: &ApiKey, _body: Vec<u8>) -> Result<ByteStream, TransportError> {
            futures_util::future::pending().await
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_initial_connection() {
        let client = ChatClient::with_transport(PendingTransport);
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.cancel();
        });
        let result = client
            .stream_chat(
                &ApiKey::new("sk-test").unwrap(),
                &request(),
                &cancellation,
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(ChatError::Cancelled)));
    }

    #[test]
    fn rejects_unbounded_sse_events() {
        let mut decoder = SseDecoder::default();
        let oversized = vec![b'x'; MAX_SSE_BUFFER_BYTES + 1];
        assert!(matches!(
            decoder.push(&oversized),
            Err(ChatError::InvalidStream)
        ));
    }

    #[test]
    fn rejects_oversized_request_bodies() {
        let request = ChatRequest::new(vec![ChatMessage::text(
            ChatRole::User,
            "x".repeat(MAX_CHAT_REQUEST_BYTES),
        )]);
        assert!(matches!(request.encoded(), Err(ChatError::Encode)));
    }

    #[test]
    fn rejects_unbounded_assembled_stream_fields() {
        let mut value = String::new();
        assert!(checked_push_str(&mut value, "woof", 4).is_ok());
        assert!(matches!(
            checked_push_str(&mut value, "!", 4),
            Err(ChatError::InvalidStream)
        ));

        let mut assembly = CompletionAssembly::default();
        for index in 0..MAX_TOOL_CALLS {
            assembly
                .tool_calls
                .insert(index, ToolCallAssembly::default());
        }
        let chunk = WireStreamChunk {
            choices: vec![WireChoice {
                index: 0,
                delta: WireDelta {
                    content: None,
                    tool_calls: vec![WireToolCallDelta {
                        index: MAX_TOOL_CALLS,
                        id: Some("call".into()),
                        function: None,
                    }],
                },
                finish_reason: None,
            }],
            usage: None,
        };
        assert!(matches!(
            assembly.apply(chunk, &mut |_| {}, &mut false),
            Err(ChatError::InvalidStream)
        ));
    }
}

use std::{collections::HashMap, future::pending, time::Duration};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{sync::mpsc, time::Instant};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        protocol::WebSocketConfig,
        Error as WebSocketError, Message,
    },
};
use zeroize::Zeroizing;

use crate::{
    validate_openai_url, ApiKey, CancellationToken, RealtimeError, REALTIME_TRANSCRIPTION_URL,
};

/// Current low-latency model from the GA Realtime transcription guide.
pub const TRANSCRIPTION_MODEL: &str = "gpt-live-transcribe";
const PCM_SAMPLE_RATE_HZ: usize = 24_000;
const PCM_CHANNEL_COUNT: usize = 1;
const MAX_CONNECT_ATTEMPTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FINAL_TRANSCRIPT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REALTIME_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUDIO_SECONDS_PER_COMMAND: usize = 4;
const MAX_AUDIO_SAMPLES_PER_COMMAND: usize =
    PCM_SAMPLE_RATE_HZ * PCM_CHANNEL_COUNT * MAX_AUDIO_SECONDS_PER_COMMAND;
const MAX_LANGUAGE_BYTES: usize = 64;
const MAX_SESSION_PROMPT_BYTES: usize = 4 * 1024;
const MAX_TRANSCRIPT_ITEMS: usize = 128;
const MAX_ITEM_ID_BYTES: usize = 512;
const MAX_TRANSCRIPT_ITEM_BYTES: usize = 1024 * 1024;
const MAX_TRANSCRIPT_TOTAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealtimeSessionConfig {
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl RealtimeSessionConfig {
    /// GA Realtime transcription `session.update` for 24 kHz mono PCM16 input.
    pub fn session_update(&self) -> Value {
        let mut transcription = serde_json::Map::new();
        transcription.insert("model".into(), Value::String(TRANSCRIPTION_MODEL.into()));
        if let Some(language) = self.language.as_ref() {
            transcription.insert(
                "languages".into(),
                Value::Array(vec![Value::String(language.clone())]),
            );
        }
        if let Some(prompt) = self.prompt.as_ref() {
            transcription.insert("prompt".into(), Value::String(prompt.clone()));
        }
        json!({
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": {
                    "input": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": PCM_SAMPLE_RATE_HZ,
                        },
                        "transcription": transcription,
                        "turn_detection": null,
                    }
                }
            }
        })
    }

    fn validate(&self) -> Result<(), RealtimeError> {
        if self
            .language
            .as_ref()
            .is_some_and(|language| !valid_language_hint(language))
            || self.prompt.as_ref().is_some_and(|prompt| {
                prompt.len() > MAX_SESSION_PROMPT_BYTES || has_forbidden_control(prompt)
            })
        {
            return Err(RealtimeError::InvalidEvent);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    /// Mono signed 16-bit samples at 24 kHz. Samples are encoded little-endian on the wire.
    AppendPcm16(Vec<i16>),
    Commit,
    Clear,
    Finish,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptionEvent {
    Started,
    Level { normalized: f32 },
    Processing,
    Partial { item_id: String, text: String },
    Done { item_id: String, text: String },
}

#[derive(Clone, Debug, Default)]
pub struct TranscriptReconciler {
    order: Vec<String>,
    partials: HashMap<String, String>,
    finals: HashMap<String, String>,
}

impl TranscriptReconciler {
    pub fn register(&mut self, item_id: &str) -> Result<(), RealtimeError> {
        if item_id.is_empty() || item_id.len() > MAX_ITEM_ID_BYTES || has_forbidden_control(item_id)
        {
            return Err(RealtimeError::InvalidEvent);
        }
        if !self.order.iter().any(|candidate| candidate == item_id) {
            if self.order.len() >= MAX_TRANSCRIPT_ITEMS {
                return Err(RealtimeError::InvalidEvent);
            }
            self.order.push(item_id.to_owned());
        }
        Ok(())
    }

    pub fn apply_delta(&mut self, item_id: &str, delta: &str) -> Result<String, RealtimeError> {
        self.register(item_id)?;
        if self.finals.contains_key(item_id)
            || delta.len()
                > MAX_TRANSCRIPT_ITEM_BYTES
                    .saturating_sub(self.partials.get(item_id).map_or(0, String::len))
            || delta.len() > MAX_TRANSCRIPT_TOTAL_BYTES.saturating_sub(self.stored_bytes())
        {
            return Err(RealtimeError::InvalidEvent);
        }
        let partial = self.partials.entry(item_id.to_owned()).or_default();
        partial.push_str(delta);
        Ok(partial.clone())
    }

    pub fn complete(&mut self, item_id: &str, transcript: &str) -> Result<String, RealtimeError> {
        self.register(item_id)?;
        let replaced_bytes = self.partials.get(item_id).map_or(0, String::len)
            + self.finals.get(item_id).map_or(0, String::len);
        let retained_bytes = self.stored_bytes().saturating_sub(replaced_bytes);
        if transcript.len() > MAX_TRANSCRIPT_ITEM_BYTES
            || transcript.len() > MAX_TRANSCRIPT_TOTAL_BYTES.saturating_sub(retained_bytes)
        {
            return Err(RealtimeError::InvalidEvent);
        }
        self.partials.remove(item_id);
        self.finals
            .insert(item_id.to_owned(), transcript.to_owned());
        Ok(transcript.to_owned())
    }

    pub fn text_for(&self, item_id: &str) -> Option<&str> {
        self.finals
            .get(item_id)
            .or_else(|| self.partials.get(item_id))
            .map(String::as_str)
    }

    pub fn final_transcript(&self) -> String {
        self.order
            .iter()
            .filter_map(|item_id| self.finals.get(item_id))
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn stored_bytes(&self) -> usize {
        self.partials.values().map(String::len).sum::<usize>()
            + self.finals.values().map(String::len).sum::<usize>()
    }
}

#[derive(Clone, Debug, Default)]
pub struct RealtimeTranscriptionClient;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishDisposition {
    CommitAndWait,
    Wait,
    Complete,
}

#[derive(Default)]
struct TurnState {
    has_uncommitted_audio: bool,
    pending_transcriptions: usize,
    finishing: bool,
}

impl TurnState {
    fn append(&mut self) {
        self.has_uncommitted_audio = true;
    }

    fn clear(&mut self) {
        self.has_uncommitted_audio = false;
    }

    fn commit(&mut self) -> bool {
        if !self.has_uncommitted_audio {
            return false;
        }
        self.has_uncommitted_audio = false;
        self.pending_transcriptions = self.pending_transcriptions.saturating_add(1);
        true
    }

    fn finish(&mut self) -> FinishDisposition {
        self.finishing = true;
        if self.commit() {
            FinishDisposition::CommitAndWait
        } else if self.pending_transcriptions > 0 {
            FinishDisposition::Wait
        } else {
            FinishDisposition::Complete
        }
    }

    fn completed(&mut self) -> bool {
        self.pending_transcriptions = self.pending_transcriptions.saturating_sub(1);
        self.finishing && self.pending_transcriptions == 0
    }
}

impl RealtimeTranscriptionClient {
    pub async fn run<F>(
        &self,
        key: &ApiKey,
        config: &RealtimeSessionConfig,
        mut audio: mpsc::Receiver<AudioCommand>,
        cancellation: &CancellationToken,
        mut callback: F,
    ) -> Result<String, RealtimeError>
    where
        F: FnMut(TranscriptionEvent) + Send,
    {
        debug_assert!(validate_openai_url(REALTIME_TRANSCRIPTION_URL, true));
        config.validate()?;
        let socket = connect_with_retry(key, cancellation).await?;
        let (mut sink, mut incoming) = socket.split();
        let session_update = serde_json::to_string(&config.session_update())
            .map_err(|_| RealtimeError::InvalidEvent)?;
        sink.send(Message::Text(session_update.into()))
            .await
            .map_err(|_| RealtimeError::Connection)?;
        callback(TranscriptionEvent::Started);

        let mut reconciler = TranscriptReconciler::default();
        let mut turn_state = TurnState::default();
        let mut final_deadline: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    let _ = sink.send(Message::Close(None)).await;
                    return Err(RealtimeError::Cancelled);
                }
                _ = async {
                    match final_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => pending::<()>().await,
                    }
                } => {
                    let _ = sink.send(Message::Close(None)).await;
                    return Err(RealtimeError::Timeout);
                }
                command = audio.recv() => {
                    let Some(command) = command else {
                        let _ = sink.send(Message::Close(None)).await;
                        return Err(RealtimeError::AudioClosed);
                    };
                    match command {
                        AudioCommand::AppendPcm16(samples) => {
                            if samples.is_empty() {
                                continue;
                            }
                            if samples.len() > MAX_AUDIO_SAMPLES_PER_COMMAND {
                                return Err(RealtimeError::InvalidEvent);
                            }
                            let level = normalized_level(&samples);
                            send_json(&mut sink, input_audio_buffer_append(&samples)).await?;
                            turn_state.append();
                            callback(TranscriptionEvent::Level { normalized: level });
                        }
                        AudioCommand::Commit => {
                            if turn_state.commit() {
                                send_json(&mut sink, input_audio_buffer_commit()).await?;
                                callback(TranscriptionEvent::Processing);
                            }
                        }
                        AudioCommand::Clear => {
                            send_json(&mut sink, input_audio_buffer_clear()).await?;
                            turn_state.clear();
                        }
                        AudioCommand::Finish => {
                            match turn_state.finish() {
                                FinishDisposition::CommitAndWait => {
                                    send_json(&mut sink, input_audio_buffer_commit()).await?;
                                    callback(TranscriptionEvent::Processing);
                                    final_deadline = Some(Instant::now() + FINAL_TRANSCRIPT_TIMEOUT);
                                }
                                FinishDisposition::Wait => {
                                    final_deadline = Some(Instant::now() + FINAL_TRANSCRIPT_TIMEOUT);
                                }
                                FinishDisposition::Complete => {
                                    let _ = sink.send(Message::Close(None)).await;
                                    return Ok(reconciler.final_transcript());
                                }
                            }
                        }
                    }
                }
                message = incoming.next() => {
                    let Some(message) = message else {
                        let final_text = reconciler.final_transcript();
                        return if final_text.is_empty() {
                            Err(RealtimeError::Connection)
                        } else {
                            Ok(final_text)
                        };
                    };
                    let message = message.map_err(|_| RealtimeError::Connection)?;
                    let Some(value) = message_json(message)? else {
                        continue;
                    };
                    let event_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
                    match event_type {
                        "conversation.item.created" | "input_audio_buffer.committed" => {
                            if let Some(item_id) = value
                                .get("item_id")
                                .and_then(Value::as_str)
                                .or_else(|| value.pointer("/item/id").and_then(Value::as_str))
                            {
                                reconciler.register(item_id)?;
                            }
                        }
                        "conversation.item.input_audio_transcription.delta" => {
                            let item_id = required_string(&value, "item_id")?;
                            let delta = required_string(&value, "delta")?;
                            let partial = reconciler.apply_delta(item_id, delta)?;
                            callback(TranscriptionEvent::Partial {
                                item_id: item_id.to_owned(),
                                text: partial,
                            });
                        }
                        "conversation.item.input_audio_transcription.completed" => {
                            let item_id = required_string(&value, "item_id")?;
                            let transcript = required_string(&value, "transcript")?;
                            let final_text = reconciler.complete(item_id, transcript)?;
                            callback(TranscriptionEvent::Done {
                                item_id: item_id.to_owned(),
                                text: final_text,
                            });
                            if turn_state.completed() {
                                let _ = sink.send(Message::Close(None)).await;
                                return Ok(reconciler.final_transcript());
                            }
                        }
                        "conversation.item.input_audio_transcription.failed" | "error" => {
                            return Err(classify_realtime_event_error(&value));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

async fn connect_with_retry(
    key: &ApiKey,
    cancellation: &CancellationToken,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    RealtimeError,
> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let request = realtime_request(key)?;
        let result = tokio::select! {
            _ = cancellation.cancelled() => return Err(RealtimeError::Cancelled),
            connected = tokio::time::timeout(
                CONNECT_TIMEOUT,
                connect_async_with_config(request, Some(realtime_socket_config()), false),
            ) => match connected {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(classify_connect_error(&error)),
                Err(_) => Err(RealtimeError::Connection),
            },
        };
        match result {
            Ok((socket, _)) => return Ok(socket),
            Err(classified) => {
                if attempt >= MAX_CONNECT_ATTEMPTS || !classified.retryable() {
                    return Err(classified);
                }
                let delay = Duration::from_millis(250 * (1_u64 << (attempt - 1)));
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(RealtimeError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

fn realtime_socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(64 * 1024)
        .max_write_buffer_size(4 * 1024 * 1024)
        .max_message_size(Some(MAX_REALTIME_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_REALTIME_MESSAGE_BYTES))
}

fn realtime_request(
    key: &ApiKey,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, RealtimeError> {
    let mut request = REALTIME_TRANSCRIPTION_URL
        .into_client_request()
        .map_err(|_| RealtimeError::Connection)?;
    let bearer = Zeroizing::new(format!("Bearer {}", key.expose()));
    let authorization =
        HeaderValue::from_bytes(bearer.as_bytes()).map_err(|_| RealtimeError::Connection)?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    Ok(request)
}

fn classify_connect_error(error: &WebSocketError) -> RealtimeError {
    match error {
        WebSocketError::Http(response) => match response.status().as_u16() {
            401 | 403 => RealtimeError::Authentication,
            429 => RealtimeError::RateLimited,
            status @ 500..=599 => RealtimeError::Server { status },
            _ => RealtimeError::Rejected,
        },
        WebSocketError::Io(_) | WebSocketError::Tls(_) => RealtimeError::Connection,
        _ => RealtimeError::Rejected,
    }
}

fn classify_realtime_event_error(value: &Value) -> RealtimeError {
    let marker = value
        .pointer("/error/code")
        .or_else(|| value.pointer("/error/type"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if marker.contains("auth") || marker.contains("api_key") {
        RealtimeError::Authentication
    } else if marker.contains("rate_limit") {
        RealtimeError::RateLimited
    } else if marker.contains("server_error") || marker.contains("internal_error") {
        RealtimeError::Server { status: 500 }
    } else {
        RealtimeError::Rejected
    }
}

async fn send_json<S>(sink: &mut S, value: Value) -> Result<(), RealtimeError>
where
    S: futures_util::Sink<Message> + Unpin,
{
    let encoded = serde_json::to_string(&value).map_err(|_| RealtimeError::InvalidEvent)?;
    if encoded.len() > MAX_REALTIME_MESSAGE_BYTES {
        return Err(RealtimeError::InvalidEvent);
    }
    sink.send(Message::Text(encoded.into()))
        .await
        .map_err(|_| RealtimeError::Connection)
}

fn message_json(message: Message) -> Result<Option<Value>, RealtimeError> {
    match message {
        Message::Text(text) => {
            if text.len() > MAX_REALTIME_MESSAGE_BYTES {
                return Err(RealtimeError::InvalidEvent);
            }
            serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| RealtimeError::InvalidEvent)
        }
        Message::Binary(bytes) => {
            if bytes.len() > MAX_REALTIME_MESSAGE_BYTES {
                return Err(RealtimeError::InvalidEvent);
            }
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| RealtimeError::InvalidEvent)
        }
        Message::Close(_) => Ok(None),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
    }
}

fn has_forbidden_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn valid_language_hint(language: &str) -> bool {
    !language.is_empty()
        && language.len() <= MAX_LANGUAGE_BYTES
        && language
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, RealtimeError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(RealtimeError::InvalidEvent)
}

fn encode_pcm16(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    BASE64.encode(bytes)
}

fn input_audio_buffer_append(samples: &[i16]) -> Value {
    json!({
        "type": "input_audio_buffer.append",
        "audio": encode_pcm16(samples),
    })
}

fn input_audio_buffer_commit() -> Value {
    json!({"type": "input_audio_buffer.commit"})
}

fn input_audio_buffer_clear() -> Value {
    json!({"type": "input_audio_buffer.clear"})
}

fn normalized_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean_square = samples
        .iter()
        .map(|sample| {
            let normalized = f64::from(*sample) / f64::from(i16::MAX);
            normalized * normalized
        })
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt().clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ga_transcription_session_matches_the_official_shape() {
        let value = RealtimeSessionConfig {
            language: Some("en".into()),
            prompt: Some("Dog names and product vocabulary".into()),
        }
        .session_update();
        assert_eq!(
            value,
            json!({
                "type": "session.update",
                "session": {
                    "type": "transcription",
                    "audio": {
                        "input": {
                            "format": {
                                "type": "audio/pcm",
                                "rate": 24_000,
                            },
                            "transcription": {
                                "model": "gpt-live-transcribe",
                                "languages": ["en"],
                                "prompt": "Dog names and product vocabulary",
                            },
                            "turn_detection": null,
                        }
                    }
                }
            })
        );
        assert_eq!(TRANSCRIPTION_MODEL, "gpt-live-transcribe");
        assert_eq!(PCM_CHANNEL_COUNT, 1);
    }

    #[test]
    fn ga_audio_buffer_events_match_the_official_shapes() {
        assert_eq!(
            input_audio_buffer_append(&[0x1234, -2]),
            json!({
                "type": "input_audio_buffer.append",
                "audio": "NBL+/w==",
            })
        );
        assert_eq!(
            input_audio_buffer_commit(),
            json!({"type": "input_audio_buffer.commit"})
        );
        assert_eq!(
            input_audio_buffer_clear(),
            json!({"type": "input_audio_buffer.clear"})
        );
    }

    #[test]
    fn ga_websocket_request_uses_only_standard_headers() {
        let key = ApiKey::new("sk-test-only").unwrap();
        let request = realtime_request(&key).unwrap();
        assert_eq!(request.uri().scheme_str(), Some("wss"));
        assert_eq!(request.uri().host(), Some("api.openai.com"));
        assert!(request.headers().contains_key(AUTHORIZATION));
        let allowed_headers = [
            "authorization",
            "connection",
            "host",
            "sec-websocket-key",
            "sec-websocket-version",
            "upgrade",
        ];
        assert!(request
            .headers()
            .keys()
            .all(|name| allowed_headers.contains(&name.as_str())));
    }

    #[test]
    fn reconciles_out_of_order_finals_by_item_id() {
        let mut state = TranscriptReconciler::default();
        assert_eq!(state.apply_delta("item_1", "Hel").unwrap(), "Hel");
        assert_eq!(state.apply_delta("item_2", "Wor").unwrap(), "Wor");
        assert_eq!(state.apply_delta("item_1", "lo").unwrap(), "Hello");
        assert_eq!(state.complete("item_2", "World.").unwrap(), "World.");
        assert_eq!(state.complete("item_1", "Hello!").unwrap(), "Hello!");
        assert_eq!(state.text_for("item_1"), Some("Hello!"));
        assert_eq!(state.final_transcript(), "Hello! World.");
    }

    #[test]
    fn final_replaces_not_appends_partial_text() {
        let mut state = TranscriptReconciler::default();
        state.apply_delta("item", "I has").unwrap();
        state.apply_delta("item", " a dog").unwrap();
        state.complete("item", "I have a dog.").unwrap();
        assert_eq!(state.final_transcript(), "I have a dog.");
    }

    #[test]
    fn rejects_unbounded_realtime_inputs() {
        let config = RealtimeSessionConfig {
            language: Some("x".repeat(MAX_LANGUAGE_BYTES + 1)),
            prompt: None,
        };
        assert!(matches!(
            config.validate(),
            Err(RealtimeError::InvalidEvent)
        ));
        assert!(matches!(
            RealtimeSessionConfig {
                language: Some("en\nfr".into()),
                prompt: None,
            }
            .validate(),
            Err(RealtimeError::InvalidEvent)
        ));

        let mut state = TranscriptReconciler::default();
        assert!(matches!(
            state.apply_delta("item", &"x".repeat(MAX_TRANSCRIPT_ITEM_BYTES + 1)),
            Err(RealtimeError::InvalidEvent)
        ));
        assert!(matches!(
            message_json(Message::Text(
                "x".repeat(MAX_REALTIME_MESSAGE_BYTES + 1).into()
            )),
            Err(RealtimeError::InvalidEvent)
        ));

        let socket = realtime_socket_config();
        assert_eq!(socket.max_message_size, Some(MAX_REALTIME_MESSAGE_BYTES));
        assert_eq!(socket.max_frame_size, Some(MAX_REALTIME_MESSAGE_BYTES));
    }

    #[test]
    fn pcm_encoding_is_little_endian_and_level_is_bounded() {
        assert_eq!(encode_pcm16(&[0x1234, -2]), "NBL+/w==");
        assert_eq!(normalized_level(&[0, 0]), 0.0);
        assert!((0.99..=1.0).contains(&normalized_level(&[i16::MAX])));
    }

    #[test]
    fn finish_waits_for_an_already_committed_turn() {
        let mut state = TurnState::default();
        state.append();
        assert!(state.commit());
        assert_eq!(state.finish(), FinishDisposition::Wait);
        assert!(state.completed());
    }

    #[test]
    fn finish_commits_uncommitted_audio_before_waiting() {
        let mut state = TurnState::default();
        state.append();
        assert_eq!(state.finish(), FinishDisposition::CommitAndWait);
        assert!(state.completed());
    }

    #[test]
    fn realtime_errors_retry_only_transient_classes() {
        assert!(RealtimeError::Connection.retryable());
        assert!(RealtimeError::RateLimited.retryable());
        assert!(RealtimeError::Server { status: 503 }.retryable());
        assert!(!RealtimeError::Server { status: 400 }.retryable());
        assert!(!RealtimeError::Authentication.retryable());
        assert!(!RealtimeError::Rejected.retryable());
        assert!(!RealtimeError::InvalidEvent.retryable());
    }

    #[test]
    fn realtime_event_errors_preserve_non_retryable_authentication() {
        assert!(matches!(
            classify_realtime_event_error(&json!({
                "type": "error",
                "error": {"code": "invalid_api_key"}
            })),
            RealtimeError::Authentication
        ));
        assert!(matches!(
            classify_realtime_event_error(&json!({
                "type": "error",
                "error": {"type": "rate_limit_error"}
            })),
            RealtimeError::RateLimited
        ));
    }
}

//! MCP stdio-to-loopback bridge.

use std::{collections::BTreeMap, future::Future, process::Stdio, sync::Arc, time::Duration};

use reqwest::{redirect::Policy, Client, StatusCode};
use serde_json::{json, Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{sleep, timeout, Instant};
use woof_core::{
    generate_health_challenge, verify_health_proof, ApiToken, HEALTH_CHALLENGE_HEADER,
    HEALTH_PROOF_HEADER,
};

pub const MCP_PROTOCOL: &str = "2025-11-25";
pub const SUPPORTED_MCP_PROTOCOLS: [&str; 4] =
    [MCP_PROTOCOL, "2025-06-18", "2025-03-26", "2024-11-05"];
pub const MCP_SERVER_NAME: &str = "woof";
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:3334";
pub const WOOF_BUNDLE_ID: &str = "com.julius.woof";

const DAEMON_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const DAEMON_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const DAEMON_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const HEALTH_RESPONSE_BODY: &[u8] = br#"{"status":"ok"}"#;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_FIRST_LINE_BYTES: usize = MAX_FRAME_BYTES + 2;
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_HEADER_LINES: usize = 64;
const MAX_LEADING_BLANK_LINES: usize = 8;
const MAX_QUERY_BYTES: usize = 1024;
const MAX_PERIOD_BYTES: usize = 64;
const MAX_SLUG_BYTES: usize = 256;
const MAX_DATE_BYTES: usize = 32;
const MAX_ENUM_BYTES: usize = 32;
const MAX_SNAPSHOT_ID_BYTES: usize = 128;
const MAX_SNAPSHOT_IDS: usize = 100;
const MAX_SNAPSHOT_IDS_QUERY_BYTES: usize = 8 * 1024;
const MAX_DAEMON_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

fn valid_protocol_version(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameMode {
    ContentLength,
    Newline,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("invalid Content-Length header")]
    ContentLength,
    #[error("MCP frame exceeds the size limit")]
    FrameTooLarge,
    #[error("stdio I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon request failed")]
    Http(#[from] reqwest::Error),
    #[error("daemon rejected the request with HTTP {0}")]
    DaemonStatus(StatusCode),
    #[error("daemon response exceeds the size limit")]
    DaemonResponseTooLarge,
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(&'static str),
    #[error("could not launch the woof app")]
    AppLaunch,
    #[error("woof local daemon did not become healthy within 20 seconds")]
    DaemonStartupTimeout,
}

pub struct McpBridge {
    daemon_url: String,
    token: Arc<ApiToken>,
    client: Client,
}

impl McpBridge {
    pub fn new(daemon_url: impl Into<String>, token: ApiToken) -> Result<Self, BridgeError> {
        let daemon_url = daemon_url.into();
        if daemon_url != DEFAULT_DAEMON_URL {
            return Err(BridgeError::InvalidArguments(
                "daemon URL must be the fixed woof loopback endpoint",
            ));
        }
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none())
            .build()?;
        Ok(Self {
            daemon_url,
            token: Arc::new(token),
            client,
        })
    }

    pub async fn serve<R, W>(&self, mut input: R, mut output: W) -> Result<(), BridgeError>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut initialized = false;
        while let Some((mode, body)) = read_frame(&mut input).await? {
            let request = match serde_json::from_slice::<Value>(&body) {
                Ok(request) => request,
                Err(error) => {
                    let response = json_rpc_error(Value::Null, -32700, "Parse error", None);
                    write_frame(&mut output, mode, &response).await?;
                    output.flush().await?;
                    let _ = error;
                    continue;
                }
            };
            if let Some(response) = self.handle_with_session(request, &mut initialized).await {
                write_frame(&mut output, mode, &response).await?;
                output.flush().await?;
            }
        }
        Ok(())
    }

    pub async fn handle(&self, request: Value) -> Option<Value> {
        // Direct handling is used by focused contract tests. Real stdio
        // sessions use `handle_with_session` and enforce initialization.
        let mut initialized = true;
        self.handle_with_session(request, &mut initialized).await
    }

    async fn handle_with_session(&self, request: Value, initialized: &mut bool) -> Option<Value> {
        let object = match request.as_object() {
            Some(object) => object,
            None => return Some(json_rpc_error(Value::Null, -32600, "Invalid Request", None)),
        };
        let id = object.get("id").cloned();
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(json_rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
                None,
            ));
        }
        let method = match object.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                return Some(json_rpc_error(
                    id.unwrap_or(Value::Null),
                    -32600,
                    "Invalid Request",
                    None,
                ))
            }
        };

        // MCP notifications are intentionally acknowledgement-free.
        let id = id?;
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));

        if method != "initialize" && method != "ping" && !*initialized {
            return Some(json_rpc_error(id, -32002, "Server not initialized", None));
        }

        let result = match method {
            "initialize" => {
                if let Some(requested) = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .filter(|version| valid_protocol_version(version))
                {
                    let negotiated = if SUPPORTED_MCP_PROTOCOLS.contains(&requested) {
                        requested
                    } else {
                        MCP_PROTOCOL
                    };
                    *initialized = true;
                    Ok(json!({
                        "capabilities": {
                            "prompts": {},
                            "resources": {},
                            "tools": {}
                        },
                        "protocolVersion": negotiated,
                        "serverInfo": {
                            "name": MCP_SERVER_NAME,
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }))
                } else {
                    Err(BridgeError::InvalidArguments(
                        "protocolVersion must be a bounded printable string",
                    ))
                }
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            "tools/call" => self.call_tool(&params).await,
            _ => return Some(json_rpc_error(id, -32601, "Method not found", None)),
        };

        Some(match result {
            Ok(result) => json!({"id": id, "jsonrpc": "2.0", "result": result}),
            Err(BridgeError::InvalidArguments(message)) => {
                json_rpc_error(id, -32602, "Invalid params", Some(json!(message)))
            }
            Err(error) => json_rpc_error(
                id,
                -32000,
                "Daemon request failed",
                Some(json!(error.to_string())),
            ),
        })
    }

    async fn call_tool(&self, params: &Value) -> Result<Value, BridgeError> {
        let (path, query) =
            prepare_tool_call_with_gate(params, || self.ensure_daemon_available()).await?;
        let url = format!("{}{}", self.daemon_url, path);
        let mut response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose_str())
            .query(&query)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(BridgeError::DaemonStatus(response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            extend_bounded(&mut body, &chunk, MAX_DAEMON_RESPONSE_BYTES)?;
        }
        let parsed: Value = serde_json::from_slice(&body)?;
        let text = serde_json::to_string_pretty(&parsed)?;
        Ok(json!({
            "content": [{
                "type": "text",
                "text": text
            }]
        }))
    }

    async fn ensure_daemon_available(&self) -> Result<(), BridgeError> {
        ensure_daemon_with(
            DAEMON_STARTUP_TIMEOUT,
            DAEMON_HEALTH_PROBE_TIMEOUT,
            DAEMON_HEALTH_POLL_INTERVAL,
            || self.daemon_healthy(),
            launch_woof_app,
        )
        .await
    }

    async fn daemon_healthy(&self) -> bool {
        let url = format!("{}/health", self.daemon_url);
        let challenge = generate_health_challenge();
        let probe = async {
            let mut response = self
                .client
                .get(url)
                .header(HEALTH_CHALLENGE_HEADER, &challenge)
                .send()
                .await
                .ok()?;
            let status = response.status();
            let proof = response
                .headers()
                .get(HEALTH_PROOF_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut body = Vec::with_capacity(HEALTH_RESPONSE_BODY.len());
            while let Some(chunk) = response.chunk().await.ok()? {
                let body_length = body.len().checked_add(chunk.len())?;
                if body_length > HEALTH_RESPONSE_BODY.len() {
                    return Some(false);
                }
                body.extend_from_slice(&chunk);
            }
            Some(health_response_is_valid(
                status,
                proof.as_deref(),
                &body,
                &self.token,
                &challenge,
            ))
        };
        matches!(
            timeout(DAEMON_HEALTH_PROBE_TIMEOUT, probe).await,
            Ok(Some(true))
        )
    }
}

fn extend_bounded(
    destination: &mut Vec<u8>,
    chunk: &[u8],
    maximum: usize,
) -> Result<(), BridgeError> {
    let next_length = destination
        .len()
        .checked_add(chunk.len())
        .ok_or(BridgeError::DaemonResponseTooLarge)?;
    if next_length > maximum {
        return Err(BridgeError::DaemonResponseTooLarge);
    }
    destination.extend_from_slice(chunk);
    Ok(())
}

fn health_response_is_valid(
    status: StatusCode,
    proof: Option<&str>,
    body: &[u8],
    token: &ApiToken,
    challenge: &str,
) -> bool {
    status == StatusCode::OK
        && body == HEALTH_RESPONSE_BODY
        && proof.is_some_and(|proof| verify_health_proof(token, challenge, proof))
}

async fn prepare_tool_call_with_gate<G, GateFuture>(
    params: &Value,
    gate: G,
) -> Result<(&'static str, BTreeMap<&'static str, String>), BridgeError>
where
    G: FnOnce() -> GateFuture,
    GateFuture: Future<Output = Result<(), BridgeError>>,
{
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or(BridgeError::InvalidArguments("missing tool name"))?;

    let arguments = match params.get("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => {
            return Err(BridgeError::InvalidArguments(
                "tool arguments must be an object",
            ))
        }
    };
    let request = tool_request(name, &arguments)?;
    gate().await?;
    Ok(request)
}

async fn launch_woof_app() -> Result<(), BridgeError> {
    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("/usr/bin/open");
        command
            .arg("-b")
            .arg(WOOF_BUNDLE_ID)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let status = timeout(Duration::from_secs(5), command.status())
            .await
            .map_err(|_| BridgeError::AppLaunch)?
            .map_err(|_| BridgeError::AppLaunch)?;
        status.success().then_some(()).ok_or(BridgeError::AppLaunch)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(BridgeError::AppLaunch)
    }
}

async fn ensure_daemon_with<P, ProbeFuture, L, LaunchFuture>(
    startup_timeout: Duration,
    probe_timeout: Duration,
    poll_interval: Duration,
    mut probe: P,
    launch: L,
) -> Result<(), BridgeError>
where
    P: FnMut() -> ProbeFuture,
    ProbeFuture: Future<Output = bool>,
    L: FnOnce() -> LaunchFuture,
    LaunchFuture: Future<Output = Result<(), BridgeError>>,
{
    if timeout(probe_timeout, probe()).await == Ok(true) {
        return Ok(());
    }

    launch().await?;
    let deadline = Instant::now() + startup_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BridgeError::DaemonStartupTimeout);
        }
        if timeout(remaining.min(probe_timeout), probe()).await == Ok(true) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BridgeError::DaemonStartupTimeout);
        }
        sleep(remaining.min(poll_interval)).await;
    }
}

pub async fn read_frame<R>(input: &mut R) -> Result<Option<(FrameMode, Vec<u8>)>, BridgeError>
where
    R: AsyncBufRead + Unpin,
{
    let mut leading_blank_lines = 0;
    loop {
        let Some(mut first_line) = read_line_limited(input, MAX_FIRST_LINE_BYTES).await? else {
            return Ok(None);
        };
        if is_blank_line(&first_line) {
            leading_blank_lines += 1;
            if leading_blank_lines > MAX_LEADING_BLANK_LINES {
                return Err(BridgeError::FrameTooLarge);
            }
            continue;
        }
        if has_content_length_prefix(&first_line) {
            if first_line.len() > MAX_HEADER_LINE_BYTES {
                return Err(BridgeError::FrameTooLarge);
            }
            let content_length = parse_content_length(&first_line)?;
            let mut header_bytes = first_line.len();
            let mut header_lines = 1;
            loop {
                let Some(header) = read_line_limited(input, MAX_HEADER_LINE_BYTES).await? else {
                    return Err(BridgeError::ContentLength);
                };
                header_bytes = header_bytes
                    .checked_add(header.len())
                    .ok_or(BridgeError::FrameTooLarge)?;
                header_lines += 1;
                if header_bytes > MAX_HEADER_BYTES || header_lines > MAX_HEADER_LINES {
                    return Err(BridgeError::FrameTooLarge);
                }
                if is_blank_line(&header) {
                    break;
                }
                if has_content_length_prefix(&header) {
                    return Err(BridgeError::ContentLength);
                }
            }
            let mut body = vec![0_u8; content_length];
            input.read_exact(&mut body).await?;
            return Ok(Some((FrameMode::ContentLength, body)));
        }

        trim_line_ending(&mut first_line);
        if first_line.len() > MAX_FRAME_BYTES {
            return Err(BridgeError::FrameTooLarge);
        }
        return Ok(Some((FrameMode::Newline, first_line)));
    }
}

async fn read_line_limited<R>(input: &mut R, maximum: usize) -> Result<Option<Vec<u8>>, BridgeError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let buffer = input.fill_buf().await?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |position| position + 1);
        let next_length = line
            .len()
            .checked_add(consumed)
            .ok_or(BridgeError::FrameTooLarge)?;
        if next_length > maximum {
            return Err(BridgeError::FrameTooLarge);
        }
        line.extend_from_slice(&buffer[..consumed]);
        input.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn has_content_length_prefix(line: &[u8]) -> bool {
    const PREFIX: &[u8] = b"content-length:";
    line.get(..PREFIX.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(PREFIX))
}

fn is_blank_line(line: &[u8]) -> bool {
    line.iter().all(|byte| byte.is_ascii_whitespace())
}

fn trim_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
}

fn parse_content_length(header: &[u8]) -> Result<usize, BridgeError> {
    let header = std::str::from_utf8(header).map_err(|_| BridgeError::ContentLength)?;
    let length = header
        .split_once(':')
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .ok_or(BridgeError::ContentLength)?;
    if length > MAX_FRAME_BYTES {
        return Err(BridgeError::FrameTooLarge);
    }
    Ok(length)
}

pub async fn write_frame<W>(
    output: &mut W,
    mode: FrameMode,
    value: &Value,
) -> Result<(), BridgeError>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    match mode {
        FrameMode::ContentLength => {
            output
                .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
                .await?;
            output.write_all(&body).await?;
        }
        FrameMode::Newline => {
            output.write_all(&body).await?;
            output.write_all(b"\n").await?;
        }
    }
    Ok(())
}

fn tool_request(
    name: &str,
    arguments: &Map<String, Value>,
) -> Result<(&'static str, BTreeMap<&'static str, String>), BridgeError> {
    let allowed_arguments =
        allowed_argument_names(name).ok_or(BridgeError::InvalidArguments("unknown tool name"))?;
    reject_unknown_arguments(arguments, allowed_arguments)?;
    let mut query = BTreeMap::new();
    let path = match name {
        "search_memory" => {
            query.insert(
                "q",
                required_string(arguments, "query", MAX_QUERY_BYTES)?.to_string(),
            );
            query.insert(
                "limit",
                bounded_integer(arguments, "limit", 20, 30)?.to_string(),
            );
            "/search"
        }
        "get_chronicle" => {
            query.insert(
                "level",
                required_enum(
                    arguments,
                    "level",
                    &["hour", "day", "week", "month", "year"],
                )?
                .to_string(),
            );
            query.insert(
                "period",
                required_string(arguments, "period", MAX_PERIOD_BYTES)?.to_string(),
            );
            "/chronicle"
        }
        "get_working_memory" => {
            query.insert(
                "limit",
                bounded_integer(arguments, "limit", 40, 200)?.to_string(),
            );
            "/working-memory"
        }
        "get_recent_activity" => {
            query.insert(
                "minutes",
                bounded_integer(arguments, "minutes", 30, 360)?.to_string(),
            );
            query.insert(
                "limit",
                bounded_integer(arguments, "limit", 12, 20)?.to_string(),
            );
            "/recent-activity"
        }
        "get_snapshots" => {
            let values = arguments
                .get("ids")
                .and_then(Value::as_array)
                .ok_or(BridgeError::InvalidArguments("ids must be an array"))?;
            if values.len() > MAX_SNAPSHOT_IDS {
                return Err(BridgeError::InvalidArguments("too many snapshot IDs"));
            }
            let mut ids = Vec::with_capacity(values.len());
            let mut joined_length = 0_usize;
            for value in values {
                let id = value.as_str().ok_or(BridgeError::InvalidArguments(
                    "snapshot IDs must be strings",
                ))?;
                if !valid_string_argument(id, MAX_SNAPSHOT_ID_BYTES) || id.contains(',') {
                    return Err(BridgeError::InvalidArguments("invalid snapshot ID"));
                }
                joined_length = joined_length
                    .checked_add(id.len())
                    .and_then(|length| length.checked_add(usize::from(!ids.is_empty())))
                    .ok_or(BridgeError::InvalidArguments("snapshot IDs are too large"))?;
                if joined_length > MAX_SNAPSHOT_IDS_QUERY_BYTES {
                    return Err(BridgeError::InvalidArguments("snapshot IDs are too large"));
                }
                ids.push(id);
            }
            query.insert("ids", ids.join(","));
            "/snapshots"
        }
        "search_wiki" => {
            query.insert(
                "q",
                required_string(arguments, "query", MAX_QUERY_BYTES)?.to_string(),
            );
            query.insert(
                "limit",
                bounded_integer(arguments, "limit", 10, 100)?.to_string(),
            );
            "/wiki/search"
        }
        "get_wiki_page" => {
            query.insert(
                "slug",
                required_string(arguments, "slug", MAX_SLUG_BYTES)?.to_string(),
            );
            "/wiki/page"
        }
        "list_wiki" => {
            if let Some(page_type) = optional_enum(
                arguments,
                "type",
                &["person", "project", "topic", "tool", "org"],
            )? {
                query.insert("type", page_type.to_string());
            }
            query.insert(
                "limit",
                bounded_integer(arguments, "limit", 50, 200)?.to_string(),
            );
            "/wiki/list"
        }
        "get_time_report" => {
            if let Some(period) = optional_enum(
                arguments,
                "period",
                &[
                    "today",
                    "yesterday",
                    "this_week",
                    "last_week",
                    "this_month",
                    "last_7_days",
                    "last_30_days",
                ],
            )? {
                query.insert("period", period.to_string());
            }
            if let Some(from) = optional_string(arguments, "from", MAX_DATE_BYTES)? {
                query.insert("from", from.to_string());
            }
            if let Some(to) = optional_string(arguments, "to", MAX_DATE_BYTES)? {
                query.insert("to", to.to_string());
            }
            "/time/report"
        }
        "list_time_rules" => "/time/rules",
        _ => return Err(BridgeError::InvalidArguments("unknown tool name")),
    };
    Ok((path, query))
}

fn required_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &'static str,
    maximum_bytes: usize,
) -> Result<&'a str, BridgeError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| valid_string_argument(value, maximum_bytes))
        .ok_or(BridgeError::InvalidArguments(key))
}

fn optional_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &'static str,
    maximum_bytes: usize,
) -> Result<Option<&'a str>, BridgeError> {
    match arguments.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if valid_string_argument(value, maximum_bytes) => {
            Ok(Some(value))
        }
        Some(_) => Err(BridgeError::InvalidArguments(key)),
    }
}

fn required_enum<'a>(
    arguments: &'a Map<String, Value>,
    key: &'static str,
    accepted: &[&str],
) -> Result<&'a str, BridgeError> {
    let value = required_string(arguments, key, MAX_ENUM_BYTES)?;
    accepted
        .contains(&value)
        .then_some(value)
        .ok_or(BridgeError::InvalidArguments(key))
}

fn optional_enum<'a>(
    arguments: &'a Map<String, Value>,
    key: &'static str,
    accepted: &[&str],
) -> Result<Option<&'a str>, BridgeError> {
    let Some(value) = optional_string(arguments, key, MAX_ENUM_BYTES)? else {
        return Ok(None);
    };
    accepted
        .contains(&value)
        .then_some(Some(value))
        .ok_or(BridgeError::InvalidArguments(key))
}

fn valid_string_argument(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

fn allowed_argument_names(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "search_memory" => &["query", "limit"],
        "get_chronicle" => &["level", "period"],
        "get_working_memory" => &["limit"],
        "get_recent_activity" => &["minutes", "limit"],
        "get_snapshots" => &["ids"],
        "search_wiki" => &["query", "limit"],
        "get_wiki_page" => &["slug"],
        "list_wiki" => &["type", "limit"],
        "get_time_report" => &["period", "from", "to"],
        "list_time_rules" => &[],
        _ => return None,
    })
}

fn reject_unknown_arguments(
    arguments: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), BridgeError> {
    if arguments
        .keys()
        .any(|argument| !allowed.contains(&argument.as_str()))
    {
        return Err(BridgeError::InvalidArguments("unknown tool argument"));
    }
    Ok(())
}

fn bounded_integer(
    arguments: &Map<String, Value>,
    key: &'static str,
    default: i64,
    maximum: i64,
) -> Result<i64, BridgeError> {
    match arguments.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or(BridgeError::InvalidArguments(key)),
    }
}

fn json_rpc_error(id: Value, code: i64, message: &'static str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error
            .as_object_mut()
            .expect("error is an object")
            .insert("data".to_string(), data);
    }
    json!({"id": id, "jsonrpc": "2.0", "error": error})
}

pub fn tool_definitions() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../docs/contracts/backend/mcp-tools.json"
    ))
    .expect("checked-in MCP tool contract must be valid JSON")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::Path,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
    };

    use super::*;

    async fn assert_invalid_tool_params(params: Value) {
        let gate_called = Arc::new(AtomicBool::new(false));
        let called = gate_called.clone();
        let result = prepare_tool_call_with_gate(&params, move || async move {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(matches!(result, Err(BridgeError::InvalidArguments(_))));
        assert!(!gate_called.load(Ordering::SeqCst));
    }

    fn test_token() -> ApiToken {
        ApiToken::parse_file(Path::new("fixture"), vec![b'a'; 64]).expect("token")
    }

    #[test]
    fn challenged_health_requires_exact_body_status_and_proof() {
        let token = test_token();
        let challenge = generate_health_challenge();
        let proof = woof_core::health_proof(&token, &challenge).expect("health proof");

        assert!(health_response_is_valid(
            StatusCode::OK,
            Some(&proof),
            HEALTH_RESPONSE_BODY,
            &token,
            &challenge,
        ));
        for invalid_body in [
            b"".as_slice(),
            br#"{"status":"ok"}
"#,
            br#"{ "status": "ok" }"#,
            br#"{"status":"ready"}"#,
        ] {
            assert!(!health_response_is_valid(
                StatusCode::OK,
                Some(&proof),
                invalid_body,
                &token,
                &challenge,
            ));
        }
        assert!(!health_response_is_valid(
            StatusCode::NO_CONTENT,
            Some(&proof),
            HEALTH_RESPONSE_BODY,
            &token,
            &challenge,
        ));
        assert!(!health_response_is_valid(
            StatusCode::OK,
            None,
            HEALTH_RESPONSE_BODY,
            &token,
            &challenge,
        ));
        assert!(!health_response_is_valid(
            StatusCode::OK,
            Some(&"0".repeat(64)),
            HEALTH_RESPONSE_BODY,
            &token,
            &challenge,
        ));
    }

    #[tokio::test]
    async fn newline_frames_are_rejected_at_the_byte_limit() {
        let mut input = vec![b'x'; MAX_FRAME_BYTES + 1];
        input.push(b'\n');
        let mut reader = tokio::io::BufReader::new(input.as_slice());
        let result = read_frame(&mut reader).await;
        assert!(matches!(result, Err(BridgeError::FrameTooLarge)));
    }

    #[tokio::test]
    async fn oversized_content_length_is_rejected_before_body_allocation() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
        let mut reader = tokio::io::BufReader::new(input.as_bytes());
        let result = read_frame(&mut reader).await;
        assert!(matches!(result, Err(BridgeError::FrameTooLarge)));
    }

    #[tokio::test]
    async fn duplicate_content_length_headers_are_rejected() {
        let input = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut reader = tokio::io::BufReader::new(input.as_slice());
        let result = read_frame(&mut reader).await;
        assert!(matches!(result, Err(BridgeError::ContentLength)));
    }

    #[test]
    fn accepted_argument_names_match_the_checked_in_schemas() {
        for definition in tool_definitions() {
            let name = definition["name"].as_str().expect("tool name");
            let schema_arguments = definition["inputSchema"]["properties"]
                .as_object()
                .expect("schema properties")
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            let accepted_arguments = allowed_argument_names(name)
                .expect("known tool")
                .iter()
                .map(|argument| (*argument).to_string())
                .collect::<BTreeSet<_>>();
            assert_eq!(accepted_arguments, schema_arguments, "{name}");
        }
    }

    #[test]
    fn absent_arguments_map_to_an_empty_object() {
        let (path, query) = tool_request("list_time_rules", &Map::new()).expect("tool request");
        assert_eq!(path, "/time/rules");
        assert!(query.is_empty());
    }

    #[tokio::test]
    async fn healthy_daemon_does_not_launch_the_app() {
        let launches = Arc::new(AtomicUsize::new(0));
        let launch_count = launches.clone();
        ensure_daemon_with(
            Duration::from_millis(20),
            Duration::from_millis(5),
            Duration::from_millis(1),
            || async { true },
            move || async move {
                launch_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .expect("healthy daemon");
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_daemon_launches_once_and_waits_for_health() {
        let healthy = Arc::new(AtomicBool::new(false));
        let probe_health = healthy.clone();
        let launch_health = healthy.clone();
        ensure_daemon_with(
            Duration::from_millis(20),
            Duration::from_millis(5),
            Duration::from_millis(1),
            move || {
                let probe_health = probe_health.clone();
                async move { probe_health.load(Ordering::SeqCst) }
            },
            move || async move {
                launch_health.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .expect("daemon became healthy");
    }

    #[tokio::test]
    async fn startup_wait_is_bounded() {
        let result = ensure_daemon_with(
            Duration::from_millis(5),
            Duration::from_millis(2),
            Duration::from_millis(1),
            || async { false },
            || async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(BridgeError::DaemonStartupTimeout)));
    }

    #[tokio::test]
    async fn missing_tool_name_is_rejected_before_the_daemon_gate() {
        let gate_called = Arc::new(AtomicBool::new(false));
        let called = gate_called.clone();
        let result = prepare_tool_call_with_gate(&json!({"arguments": "invalid"}), move || {
            let called = called.clone();
            async move {
                called.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
        assert!(matches!(result, Err(BridgeError::InvalidArguments(_))));
        assert!(!gate_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn invalid_tool_calls_do_not_start_or_probe_the_daemon() {
        for params in [
            json!({"name": "unknown", "arguments": {}}),
            json!({"name": "list_time_rules", "arguments": "invalid"}),
        ] {
            let gate_called = Arc::new(AtomicBool::new(false));
            let called = gate_called.clone();
            let result = prepare_tool_call_with_gate(&params, move || {
                let called = called.clone();
                async move {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                }
            })
            .await;
            assert!(matches!(result, Err(BridgeError::InvalidArguments(_))));
            assert!(!gate_called.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn present_tool_arguments_must_be_an_object() {
        for arguments in [json!(null), json!("not-an-object"), json!([]), json!(7)] {
            assert_invalid_tool_params(json!({
                "name": "list_time_rules",
                "arguments": arguments
            }))
            .await;
        }
    }

    #[tokio::test]
    async fn every_tool_rejects_unknown_arguments() {
        for definition in tool_definitions() {
            assert_invalid_tool_params(json!({
                "name": definition["name"],
                "arguments": {"unexpected": true}
            }))
            .await;
        }
    }

    #[tokio::test]
    async fn string_and_snapshot_id_arguments_are_bounded() {
        for params in [
            json!({"name": "search_memory", "arguments": {"query": "x".repeat(MAX_QUERY_BYTES + 1)}}),
            json!({"name": "search_wiki", "arguments": {"query": "x".repeat(MAX_QUERY_BYTES + 1)}}),
            json!({"name": "get_chronicle", "arguments": {"level": "day", "period": "x".repeat(MAX_PERIOD_BYTES + 1)}}),
            json!({"name": "get_wiki_page", "arguments": {"slug": "x".repeat(MAX_SLUG_BYTES + 1)}}),
            json!({"name": "get_time_report", "arguments": {"from": "x".repeat(MAX_DATE_BYTES + 1)}}),
            json!({"name": "get_time_report", "arguments": {"to": "x".repeat(MAX_DATE_BYTES + 1)}}),
            json!({"name": "search_memory", "arguments": {"query": "line\nbreak"}}),
            json!({"name": "get_snapshots", "arguments": {"ids": ["x".repeat(MAX_SNAPSHOT_ID_BYTES + 1)]}}),
            json!({"name": "get_snapshots", "arguments": {"ids": ["two,ids"]}}),
            json!({"name": "get_snapshots", "arguments": {"ids": vec!["x".repeat(100); MAX_SNAPSHOT_IDS]}}),
        ] {
            assert_invalid_tool_params(params).await;
        }
    }

    #[tokio::test]
    async fn wrong_optional_argument_types_and_enum_values_are_rejected() {
        for params in [
            json!({"name": "list_wiki", "arguments": {"type": 7}}),
            json!({"name": "list_wiki", "arguments": {"type": "invalid"}}),
            json!({"name": "get_time_report", "arguments": {"period": 7}}),
            json!({"name": "get_time_report", "arguments": {"period": "invalid"}}),
            json!({"name": "get_time_report", "arguments": {"from": false}}),
            json!({"name": "get_time_report", "arguments": {"to": []}}),
            json!({"name": "get_chronicle", "arguments": {"level": "invalid", "period": "x"}}),
        ] {
            assert_invalid_tool_params(params).await;
        }
    }

    #[tokio::test]
    async fn limits_outside_the_daemon_contract_are_rejected() {
        for params in [
            json!({"name": "search_memory", "arguments": {"query": "x", "limit": 31}}),
            json!({"name": "get_working_memory", "arguments": {"limit": 201}}),
            json!({"name": "get_recent_activity", "arguments": {"minutes": 361}}),
            json!({"name": "get_recent_activity", "arguments": {"limit": 21}}),
            json!({"name": "search_wiki", "arguments": {"query": "x", "limit": 101}}),
            json!({"name": "list_wiki", "arguments": {"limit": 201}}),
            json!({"name": "get_snapshots", "arguments": {"ids": vec!["x"; 101]}}),
        ] {
            assert_invalid_tool_params(params).await;
        }
    }

    #[test]
    fn daemon_response_limit_is_checked_before_appending() {
        let mut body = b"1234".to_vec();
        extend_bounded(&mut body, b"56", 6).expect("response remains within the limit");
        assert_eq!(body, b"123456");

        let before = body.clone();
        assert!(matches!(
            extend_bounded(&mut body, b"7", 6),
            Err(BridgeError::DaemonResponseTooLarge)
        ));
        assert_eq!(body, before);
    }
}

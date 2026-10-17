//! Native agent-server lane (`lane: "serve"`).
//!
//! Some agents ship a first-party HTTP server that is richer than the
//! generic protocol bridges built on top of it — OpenCode's own `serve`
//! process is the canonical example: its `acp` binary is itself a client of
//! this server's event feed, and high-volume sessions can overflow that
//! internal subscriber queue. Driving the REST surface directly with
//! request/response polling never creates that queue, so this lane is the
//! preferred transport where the agent offers it and the protocol lanes
//! remain the portable fallback.
//!
//! Startup discovery: the child announces `server listening on <url>` and
//! `server password <secret>` on stdout; the lane reads those lines before
//! issuing requests. Sessions are created and driven over the versioned
//! REST API with HTTP basic auth:
//!
//! - `POST /api/session` — create a session in the server's working
//!   directory (the lane spawns the server with the session's cwd).
//! - `POST /api/session/:id/model` — apply a `provider/model` selection.
//! - `POST /api/session/:id/prompt` — submit one turn; the API returns the
//!   accepted user message immediately and the turn runs asynchronously.
//! - `GET  /api/session/:id/message` — poll until the terminal `idle`
//!   record reports the turn's outcome; assistant parts carry the text.
//! - `POST /api/session/:id/interrupt` — cancel the in-flight turn.
//!
//! MCP servers declared on the lane are merged into the server's
//! `OPENCODE_CONFIG_CONTENT` document (`mcp.servers`) before spawn — the
//! server connects them lazily per session, so the lane needs no MCP
//! protocol machinery of its own.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{AgentEvent, CommandSpec, SessionRequest, TokenUsage, TurnOutcome, TurnRequest};
use crate::observe::{Direction, SharedObserver};
use crate::supervision::SupervisedChild;
use crate::transport::{base64, stdio};

/// Lines the lane scans for discovery; the server prints exactly two
/// relevant lines among its startup banner.
const DISCOVERY_LINES: usize = 128;

/// One HTTP request's response cap — message histories are the largest
/// payload and stay well under this in practice.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// How a turn failure surfaces in the message feed's terminal record.
#[derive(Debug)]
struct IdleRecord {
    outcome: String,
    error: Option<String>,
}

/// The spawned server's discovery result.
struct ServerEndpoint {
    base: String,
    auth: String,
}

/// Minimal HTTP/1.1 client for the loopback server: one fresh connection
/// per request keeps the code free of connection-pool state, which matters
/// because a hung server must never poison a later request.
struct HttpClient {
    base: String,
    auth: String,
}

impl HttpClient {
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, AdapterError> {
        let address = self
            .base
            .strip_prefix("http://")
            .unwrap_or(&self.base)
            .to_owned();
        let mut stream = tokio::net::TcpStream::connect(&address)
            .await
            .map_err(|error| {
                AdapterError::io("agent_protocol_failed", "session/transport", error)
            })?;
        let payload = body.map(|body| body.to_string()).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic {}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.auth,
            payload.len(),
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| {
                AdapterError::io("agent_protocol_failed", "session/transport", error)
            })?;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = stream.read(&mut chunk).await.map_err(|error| {
                AdapterError::io("agent_protocol_failed", "session/transport", error)
            })?;
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..read]);
            if raw.len() > MAX_RESPONSE_BYTES {
                return Err(AdapterError::permanent(
                    "agent_output_overflow",
                    "session/transport",
                ));
            }
        }
        parse_response(&raw)
    }
}

/// Split the raw response into status code and decoded body. Transfer
/// encodings the server uses — `chunked` — are decoded; a `close` framed
/// body arrives intact because the connection ends after the response.
fn parse_response(raw: &[u8]) -> Result<Value, AdapterError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            AdapterError::permanent("agent_protocol_failed", "session/transport")
                .with_detail("response headers never terminated")
        })?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let raw_body = &raw[split + 4..];
    let body = if head.to_lowercase().contains("transfer-encoding: chunked") {
        decode_chunked(raw_body)?
    } else {
        raw_body.to_vec()
    };
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(&body);
        return Err(
            AdapterError::permanent("agent_protocol_failed", "session/transport").with_detail(
                format!(
                    "server answered HTTP {status}: {}",
                    bounds::bounded_prefix(&text, Some(4096))
                ),
            ),
        );
    }
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|error| {
        AdapterError::permanent("agent_protocol_failed", "session/transport")
            .with_detail(format!("response body is not JSON: {error}"))
    })
}

/// Decode a chunked transfer body. Returns the unconsumed prefix when the
/// stream ends mid-chunk — the terminating `0`-chunk is the contract, and
/// anything else means the server dropped the connection early.
fn decode_chunked(raw: &[u8]) -> Result<Vec<u8>, AdapterError> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut cursor = raw;
    loop {
        let line_end = cursor
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| {
                AdapterError::permanent("agent_protocol_failed", "session/transport")
                    .with_detail("chunked body truncated inside a size line")
            })?;
        let size_text = String::from_utf8_lossy(&cursor[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| {
            AdapterError::permanent("agent_protocol_failed", "session/transport")
                .with_detail(format!("invalid chunk size {size_text:?}"))
        })?;
        cursor = &cursor[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        if cursor.len() < size + 2 {
            return Err(
                AdapterError::permanent("agent_protocol_failed", "session/transport")
                    .with_detail("chunked body truncated inside a chunk"),
            );
        }
        decoded.extend_from_slice(&cursor[..size]);
        cursor = &cursor[size + 2..];
    }
}

/// Merge the lane's declared MCP servers into the server configuration the
/// spec environment carries: `mcp.servers.<name>` gets a `local` entry with
/// the stdio command line and environment. ACP-style entries
/// (`{name, command, args, env:[{name,value}]}`) translate to the server
/// config shape; entries already in the server shape pass through.
fn merge_mcp_servers(env: &mut BTreeMap<String, std::ffi::OsString>, servers: &[Value]) {
    if servers.is_empty() {
        return;
    }
    let key = "OPENCODE_CONFIG_CONTENT";
    let mut document: Value = env
        .get(key)
        .and_then(|value| serde_json::from_str(&value.to_string_lossy()).ok())
        .unwrap_or_else(|| json!({}));
    let declared = document
        .pointer_mut("/mcp/servers")
        .and_then(Value::as_object_mut)
        .map(|_| true)
        .is_some();
    if !declared {
        document["mcp"] = json!({"servers": {}});
    }
    let registry = document["mcp"]["servers"].as_object_mut().unwrap();
    for server in servers {
        let Some(name) = server["name"].as_str() else {
            continue;
        };
        if server.get("type").and_then(Value::as_str) == Some("local") {
            registry.insert(name.to_owned(), server.clone());
            continue;
        }
        let mut command = vec![server["command"].as_str().unwrap_or_default().to_owned()];
        command.extend(
            server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        let environment: serde_json::Map<String, Value> = server["env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some((
                    entry["name"].as_str()?.to_owned(),
                    Value::String(entry["value"].as_str()?.to_owned()),
                ))
            })
            .collect();
        registry.insert(
            name.to_owned(),
            json!({
                "type": "local",
                "command": command,
                "environment": environment,
                "enabled": true,
            }),
        );
    }
    env.insert(
        key.to_owned(),
        std::ffi::OsString::from(document.to_string()),
    );
}

/// Read the server's startup output until both discovery lines arrive:
/// `server listening on <url>` and `server password <secret>`.
async fn discover<R>(stdout: R, observer: SharedObserver) -> Result<ServerEndpoint, AdapterError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    let mut base = None;
    let mut password = None;
    for _ in 0..DISCOVERY_LINES {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                return Err(AdapterError::io(
                    "agent_protocol_failed",
                    "session/open",
                    error,
                ));
            }
        };
        if let Some(observer) = observer.as_ref() {
            observer.record("stdout", Direction::Received, line.as_bytes());
        }
        if let Some(url) = line.strip_prefix("server listening on ") {
            base = Some(url.trim().to_owned());
        } else if let Some(secret) = line.strip_prefix("server password ") {
            password = Some(secret.trim().to_owned());
        }
        if base.is_some() && password.is_some() {
            break;
        }
    }
    let (base, secret) = match (base, password) {
        (Some(base), Some(secret)) => (base, secret),
        _ => {
            return Err(
                AdapterError::permanent("agent_protocol_failed", "session/open")
                    .with_detail("server never announced its endpoint and credential"),
            )
        }
    };
    Ok(ServerEndpoint {
        auth: base64(format!("opencode:{secret}").as_bytes()),
        base,
    })
}

/// The serve lane: drives the agent's native HTTP server.
pub struct ServeLane {
    pub max_output_bytes: Option<usize>,
    pub command: CommandSpec,
    pub mcp_servers: Vec<Value>,
    pub client_name: String,
    pub secrets: Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
}

impl ServeLane {
    /// Spawn the server, discover its endpoint, create the session and
    /// apply the session-phase selectors.
    pub async fn open(
        &self,
        request: SessionRequest,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<ServeSession, AdapterError> {
        let mut env = crate::env::resolve(&self.command.env, &request.env, &*self.secrets).await?;
        merge_mcp_servers(&mut env, &self.mcp_servers);
        let mut command = SupervisedChild::command(&self.command, &env, request.cwd.as_deref());
        let mut child = SupervisedChild::spawn(&mut command, stdio::PROTOCOL_IO)?;
        let (stdin, stdout, stderr) = stdio::protocol_halves(&mut child)?;
        drop(stdin);
        if let Some(stderr) = stderr {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        let endpoint = match discover(stdout, self.observer.clone()).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = child.terminate().await;
                return Err(error);
            }
        };
        let http = HttpClient {
            base: endpoint.base,
            auth: endpoint.auth,
        };
        let created = match http.call("POST", "/api/session", Some(&json!({}))).await {
            Ok(created) => created,
            Err(error) => {
                let _ = child.terminate().await;
                return Err(error);
            }
        };
        let session_id = created["data"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if session_id.is_empty() {
            let _ = child.terminate().await;
            return Err(
                AdapterError::permanent("agent_protocol_failed", "session/open")
                    .with_detail("server returned no session id"),
            );
        }
        // A requested permission preset has no per-session API on this
        // lane; it is honoured only when the configuration the spec itself
        // injects verifiably denies every mutating tool — the strictest
        // possible reading of a read-only request. Anything else fails
        // closed so the caller can fall back to a lane that can enforce it.
        if let Some(permission) = request.config.permission.as_deref() {
            let locked = env
                .get("OPENCODE_CONFIG_CONTENT")
                .and_then(|value| serde_json::from_str::<Value>(&value.to_string_lossy()).ok());
            if permission != "read-only" || !locked.is_some_and(|doc| config_denies_mutation(&doc))
            {
                let _ = child.terminate().await;
                return Err(AdapterError::permanent(
                    "agent_permission_unsupported",
                    "session/configuration",
                ));
            }
        }
        let session = ServeSession {
            http,
            session_id: session_id.clone(),
            child: Mutex::new(child),
            events,
            max_output_bytes: self.max_output_bytes,
            cancelled: AtomicBool::new(false),
        };
        if let Some(model) = request.config.model.as_deref() {
            session.apply_model(model).await?;
        }
        Ok(session)
    }
}

/// The injected server config denies mutation when its permission rules
/// reject the editing actions and its tool table disables the mutation and
/// shell surfaces — the audit posture expressed as a verifiable document.
fn config_denies_mutation(document: &Value) -> bool {
    let permissions = [
        "edit",
        "bash",
        "webfetch",
        "doom_loop",
        "external_directory",
    ]
    .iter()
    .all(|rule| document["permission"][rule].as_str() == Some("deny"));
    let tools = [
        "write",
        "edit",
        "patch",
        "bash",
        "webfetch",
        "websearch",
        "task",
        "skill",
        "question",
    ]
    .iter()
    .all(|tool| document["tools"][tool].as_bool() == Some(false));
    permissions && tools
}

/// One open session on a native server.
pub struct ServeSession {
    http: HttpClient,
    session_id: String,
    child: Mutex<SupervisedChild>,
    events: mpsc::UnboundedSender<AgentEvent>,
    max_output_bytes: Option<usize>,
    /// Set when a turn observed an interrupt; the poll loop treats the
    /// terminal record that follows as a cancellation.
    cancelled: AtomicBool,
}

impl ServeSession {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// `provider/model` → `{model:{id, providerID}}`.
    async fn apply_model(&self, model: &str) -> Result<(), AdapterError> {
        let (provider, id) = model.split_once('/').unwrap_or(("opencode", model));
        let body = json!({"model": {"id": id, "providerID": provider}});
        self.http
            .call(
                "POST",
                &format!("/api/session/{}/model", self.session_id),
                Some(&body),
            )
            .await
            .map_err(|mut error| {
                error.stage = "session/set_model";
                error
            })?;
        Ok(())
    }

    /// Submit one turn: apply the requested model, post the prompt, then
    /// poll the message feed until the terminal `idle` record reports the
    /// outcome. Assistant text collected along the way becomes the turn's
    /// output.
    pub async fn prompt(&self, request: TurnRequest) -> Result<TurnOutcome, AdapterError> {
        if let Some(model) = request.model.as_deref() {
            self.apply_model(model).await?;
        }
        let marker = self
            .http
            .call(
                "POST",
                &format!("/api/session/{}/prompt", self.session_id),
                Some(&json!({"text": request.text})),
            )
            .await?;
        // Bound this turn by the prompt record's creation time, not its id:
        // the message feed paginates and the user record ages out of the
        // newest page on long turns.
        let marker_ms = marker["data"]["time"]["created"].as_u64().unwrap_or(0);
        // The poll is the turn: no internal deadline, the caller's own
        // supervision decides how long a viable turn may run.
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let feed = self
                .http
                .call(
                    "GET",
                    &format!("/api/session/{}/message", self.session_id),
                    None,
                )
                .await?;
            let messages = feed["data"].as_array().cloned().unwrap_or_default();
            match terminal(&messages, marker_ms) {
                Some(IdleRecord { outcome, .. }) if outcome == "succeeded" => {
                    let (text, usage) = collect_output(&messages, marker_ms);
                    // No stream exists on this lane: deliver the completed
                    // text as one chunk and the terminal usage record so
                    // observers see the same event shape other lanes emit.
                    if !text.is_empty() {
                        let _ = self
                            .events
                            .send(AgentEvent::MessageChunk { text: text.clone() });
                    }
                    if let Some(usage) = usage.clone() {
                        let _ = self.events.send(AgentEvent::Usage { usage, raw: None });
                    }
                    return Ok(self.outcome(text, usage));
                }
                Some(IdleRecord { .. }) if self.cancelled.load(Ordering::Acquire) => {
                    return Err(AdapterError::permanent(
                        "agent_session_cancelled",
                        "session/prompt",
                    ));
                }
                Some(IdleRecord { error, .. }) => {
                    return Err(
                        AdapterError::permanent("agent_protocol_failed", "session/prompt")
                            .with_detail(error.unwrap_or_else(|| "turn failed".to_owned())),
                    );
                }
                None => {}
            }
        }
    }

    /// Bound the collected text to the spec's output budget and attach the
    /// last reported usage.
    fn outcome(&self, text: String, usage: Option<TokenUsage>) -> TurnOutcome {
        let mut truncated = false;
        let text = match self.max_output_bytes {
            Some(budget) => {
                let prefix = bounds::bounded_prefix(&text, Some(budget)).to_owned();
                truncated = prefix.len() < text.len();
                prefix
            }
            None => text,
        };
        TurnOutcome {
            stop_reason: "end_turn".to_owned(),
            text,
            truncated,
            exit_code: None,
            usage,
        }
    }

    /// Interrupt the in-flight turn; the poll loop reports the terminal
    /// record that follows as the failed outcome it is.
    pub async fn cancel(&self) -> Result<(), AdapterError> {
        self.cancelled.store(true, Ordering::Release);
        self.http
            .call(
                "POST",
                &format!("/api/session/{}/interrupt", self.session_id),
                Some(&json!({})),
            )
            .await?;
        Ok(())
    }

    /// The server process owns the session store; terminate it.
    pub async fn close(&self) -> Result<(), AdapterError> {
        let mut child = self.child.lock().await;
        child.terminate().await?;
        Ok(())
    }
}

/// A record's creation time in milliseconds; records without a timestamp
/// count as the newest so they stay visible.
fn created_ms(message: &Value) -> u64 {
    message["time"]["created"].as_u64().unwrap_or(u64::MAX)
}

/// The terminal `idle` record covering this turn, if it has arrived yet.
/// Messages stream newest-first; only records at or after `marker_ms` (the
/// prompt's creation time) belong to this turn — a stale `idle` from an
/// earlier turn cannot close the poll.
fn terminal(messages: &[Value], marker_ms: u64) -> Option<IdleRecord> {
    for message in messages {
        if created_ms(message) < marker_ms {
            break;
        }
        if message["type"].as_str() == Some("idle") {
            return Some(IdleRecord {
                outcome: message["outcome"].as_str().unwrap_or("failed").to_owned(),
                error: message["error"]["message"].as_str().map(str::to_owned),
            });
        }
    }
    None
}

/// Accumulate assistant text and the last usage record from the completed
/// turn — the assistant records created at or after `marker_ms`.
fn collect_output(messages: &[Value], marker_ms: u64) -> (String, Option<TokenUsage>) {
    let mut text = String::new();
    let mut usage = None;
    // Feed order is newest-first; walk it in reverse for chronological
    // output. Records older than the marker belong to earlier turns.
    let mut parts: Vec<&Value> = messages
        .iter()
        .filter(|message| {
            created_ms(message) >= marker_ms && message["type"].as_str() == Some("assistant")
        })
        .collect();
    parts.reverse();
    for message in parts {
        for part in message["content"].as_array().into_iter().flatten() {
            if part["type"].as_str() == Some("text") {
                if let Some(chunk) = part["text"].as_str() {
                    text.push_str(chunk);
                }
            }
        }
        let tokens = &message["tokens"];
        if tokens.is_object() {
            usage = Some(TokenUsage {
                input: tokens["input"].as_u64(),
                cached_input: tokens["cache"]["read"].as_u64(),
                cached_write: tokens["cache"]["write"].as_u64(),
                output: tokens["output"].as_u64(),
                thought: tokens["reasoning"].as_u64(),
                total: None,
            });
        }
    }
    (text, usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_bodies_decode() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n";
        let value = parse_response(raw).unwrap();
        assert_eq!(value, json!({"a": 1}));
    }

    #[test]
    fn chunked_truncation_fails_closed() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n{\"a\"";
        assert!(parse_response(raw).is_err());
    }

    #[test]
    fn non_2xx_is_a_protocol_failure() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found";
        let error = parse_response(raw).unwrap_err();
        assert_eq!(error.code, "agent_protocol_failed");
    }

    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"opencode:secret"), "b3BlbmNvZGU6c2VjcmV0");
    }

    #[test]
    fn acp_style_mcp_entries_translate_to_server_shape() {
        let mut env = BTreeMap::new();
        env.insert(
            "OPENCODE_CONFIG_CONTENT".to_owned(),
            std::ffi::OsString::from(r#"{"enabled_providers":["opencode"]}"#),
        );
        merge_mcp_servers(
            &mut env,
            &[json!({
                "type": "stdio", "name": "audit",
                "command": "/bin/tool", "args": ["serve", "--run", "r1"],
                "env": [{"name": "K", "value": "V"}],
            })],
        );
        let document: Value =
            serde_json::from_str(&env["OPENCODE_CONFIG_CONTENT"].to_string_lossy()).unwrap();
        assert_eq!(document["enabled_providers"][0], "opencode");
        let entry = &document["mcp"]["servers"]["audit"];
        assert_eq!(entry["type"], "local");
        assert_eq!(
            entry["command"],
            json!(["/bin/tool", "serve", "--run", "r1"])
        );
        assert_eq!(entry["environment"]["K"], "V");
        assert_eq!(entry["enabled"], true);
    }

    #[test]
    fn the_terminal_record_is_the_idle_at_or_after_the_prompt_marker() {
        // Newest-first feed; marker 100 separates this turn from the last.
        let messages = vec![
            json!({"type": "idle", "outcome": "succeeded", "time": {"created": 120}}),
            json!({"id": "m1", "type": "assistant", "time": {"created": 110},
                   "content": [{"type": "text", "text": "hi"}]}),
            json!({"id": "u1", "type": "user", "time": {"created": 100}}),
            json!({"type": "idle", "outcome": "failed", "time": {"created": 90}}),
        ];
        let record = terminal(&messages, 100).unwrap();
        assert_eq!(record.outcome, "succeeded");
        // No idle at or after the marker: this turn is still running, and
        // the older turn's terminal record must not close the poll.
        let running = vec![
            json!({"id": "a1", "type": "assistant", "time": {"created": 105}}),
            json!({"id": "u2", "type": "user", "time": {"created": 100}}),
            json!({"type": "idle", "outcome": "succeeded", "time": {"created": 90}}),
        ];
        assert!(terminal(&running, 100).is_none());
        // A paginated page whose user record aged out still yields the
        // turn's output and usage.
        let (text, usage) = collect_output(&messages[..2], 100);
        assert_eq!(text, "hi");
        assert!(usage.is_none());
        let with_tokens = vec![
            json!({"type": "idle", "outcome": "succeeded", "time": {"created": 120}}),
            json!({"id": "m1", "type": "assistant", "time": {"created": 110},
                   "content": [{"type": "text", "text": "hi"}],
                   "tokens": {"input": 10, "output": 4, "reasoning": 1,
                              "cache": {"read": 6, "write": 2}}}),
            json!({"id": "u1", "type": "user", "time": {"created": 100}}),
        ];
        let (_, usage) = collect_output(&with_tokens, 100);
        let usage = usage.unwrap();
        assert_eq!(usage.input, Some(10));
        assert_eq!(usage.cached_input, Some(6));
    }
}

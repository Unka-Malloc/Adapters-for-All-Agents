//! Argv-driven CLI lane: the fallback for agents without a protocol driver.
//!
//! One turn = one spawned process. The prompt travels either through an
//! `{prompt}` argument placeholder or through stdin; output streams back as
//! `MessageChunk` events byte-for-byte — the lane imposes no format, schema,
//! or validation on agent output (zero-assumption: natural language is a
//! legal reply).
//!
//! Two stream shapes: `Stdio` pipes, or `Pty` (Unix) for CLIs that only
//! stream correctly on a terminal. PTY selection degrades to Stdio
//! off-Unix.

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{
    AgentEvent, BindingReceiptShape, CliLaneSpec, CommandSpec, PromptChannel, SessionRequest,
    StreamMode, TokenUsage, TurnOutcome,
};
use crate::observe::{Direction, SharedObserver};
use crate::supervision::{IoPlan, StdioKind, SupervisedChild};
use crate::template::{extract_path, substitute, vars, VarSpec, Vars};

pub struct CliLane {
    pub command: CommandSpec,
    pub lane: CliLaneSpec,
    pub secrets: std::sync::Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
    pub max_output_bytes: Option<usize>,
    /// Fired by `AgentSession::cancel`; the in-flight turn's process tree is
    /// terminated. Explicit user cancellation only — transport failures and
    /// observer disconnects never reach this.
    pub cancelled: std::sync::Arc<tokio::sync::Notify>,
    /// Session binding captured from a `sessionIdPath` match on stdout —
    /// the first observed value wins and later turns resume through it.
    pub observed: std::sync::Arc<std::sync::OnceLock<String>>,
}

impl CliLane {
    /// Run the declared session-creation command (`open_args`) once when no
    /// resume binding exists; the first non-empty stdout line becomes the
    /// observed session binding (for example `cursor-agent create-chat`).
    pub async fn open_binding(&self, request: &SessionRequest) -> Result<(), AdapterError> {
        let requested = request
            .resume
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
        if self.lane.open_args.is_empty() || requested || self.observed.get().is_some() {
            return Ok(());
        }
        let cwd = request
            .cwd
            .clone()
            .or_else(|| self.command.cwd.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| std::env::temp_dir().display().to_string());
        let (args, _) = substitute_args(
            &self.lane.open_args,
            &vars(VarSpec {
                cwd: &cwd,
                ..Default::default()
            }),
        );
        let mut spec = self.command.clone();
        spec.args.extend(args);
        let env = crate::env::resolve(&spec.env, &request.env, &*self.secrets).await?;
        let mut command = SupervisedChild::command(&spec, &env, request.cwd.as_deref());
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = SupervisedChild::spawn(
            &mut command,
            IoPlan {
                stdin: StdioKind::Preserve,
                stdout: StdioKind::Preserve,
                stderr: StdioKind::Preserve,
            },
        )?;
        if let Some(stderr) = child.stderr() {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        let Some(mut stdout) = child.stdout() else {
            let _ = child.terminate().await;
            return Err(AdapterError::permanent(
                "agent_pipe_unavailable",
                "cli/stdio",
            ));
        };
        let mut output = Vec::new();
        let read = stdout
            .read_to_end(&mut output)
            .await
            .map_err(|e| AdapterError::io("agent_output_read_failed", "cli/stdout", e));
        let _ = child.terminate().await;
        read?;
        let id = output
            .split(|byte| *byte == b'\n')
            .map(|line| String::from_utf8_lossy(line).trim().to_owned())
            .find(|line| !line.is_empty());
        match id {
            Some(id) => {
                let _ = self.observed.set(id);
                Ok(())
            }
            None => Err(AdapterError::permanent(
                "agent_session_id_missing",
                "cli/open",
            )),
        }
    }

    /// Run one turn to completion. `events` receives output chunks as they
    /// arrive; nothing is buffered beyond the configured caps.
    pub async fn run_turn(
        &self,
        request: &SessionRequest,
        prompt: &str,
        model: Option<&str>,
        effort: Option<&str>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        // The effective session binding: an explicitly requested resume
        // first, then whatever the agent already reported on stdout or a
        // session-creation command.
        let binding = request
            .resume
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| self.observed.get().cloned());
        // An explicitly requested binding the CLI cannot express fails
        // closed — silently running a fresh process would lose the host's
        // session. `resume_args` or a `{sessionId}` placeholder inside the
        // argv template both count as an expressible binding.
        let resume_expressible = !self.lane.resume_args.is_empty()
            || self.lane.args.iter().any(|arg| arg.contains("{sessionId}"));
        if request
            .resume
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
            && !resume_expressible
        {
            return Err(AdapterError::permanent(
                "agent_session_resume_unsupported",
                "cli/args",
            ));
        }
        let mut args = self.lane.args.clone();
        if binding.is_some() && !self.lane.resume_args.is_empty() {
            args.extend(self.lane.resume_args.iter().cloned());
        }
        let model = model.or(request.config.model.as_deref());
        let effort = effort.or(request.config.effort.as_deref());
        let instructions = request.config.instructions.as_deref();
        let permission = request.config.permission.as_deref();
        let cwd = request
            .cwd
            .clone()
            .or_else(|| self.command.cwd.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| std::env::temp_dir().display().to_string());
        let turn_vars = vars(VarSpec {
            session_id: binding.as_deref(),
            cwd: &cwd,
            prompt,
            model,
            effort,
            mode: request.config.mode.as_deref(),
            instructions,
            permission,
            provider: request.config.provider.as_deref(),
            ..Default::default()
        });
        let (args, prompt_via_stdin) = substitute_args(&args, &turn_vars);
        let write_stdin = match self.lane.prompt_channel {
            PromptChannel::Stdin => true,
            PromptChannel::ArgOnly if !prompt_via_stdin => false,
            PromptChannel::ArgOnly => {
                return Err(AdapterError::permanent(
                    "agent_prompt_channel_unsatisfied",
                    "cli/args",
                ));
            }
            PromptChannel::ArgOrStdin => prompt_via_stdin,
        };
        let stdin_payload = write_stdin.then(|| match &self.lane.stdin_template {
            Some(template) => {
                let mut rendered = serde_json::to_string(&substitute(template, &turn_vars))
                    .unwrap_or_else(|_| template.to_string());
                rendered.push('\n');
                rendered
            }
            None => {
                let mut payload = prompt.to_owned();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                payload
            }
        });
        let mut spec = self.command.clone();
        // The command's own args are the fixed spawn prefix (subcommands,
        // wrapper flags); the lane's resolved template follows them.
        spec.args.extend(args);
        let mut env = crate::env::resolve(&spec.env, &request.env, &*self.secrets).await?;
        // A `binding_file_env` lane mints the scratch receipt path the
        // agent's installed hook writes this turn's session identity to.
        let receipt = self.lane.binding_file_env.as_ref().map(|name| {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|span| span.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "a4aa-receipt-{}-{:x}.json",
                std::process::id(),
                nonce
            ));
            env.insert(name.clone(), path.as_os_str().to_os_string());
            path
        });
        let command = SupervisedChild::command(&spec, &env, request.cwd.as_deref());

        let mut outcome = match effective_stream(self.lane.stream) {
            StreamMode::Stdio => self.run_stdio(command, stdin_payload, events).await,
            #[cfg(unix)]
            StreamMode::Pty => self.run_pty(command, stdin_payload, events).await,
            #[cfg(not(unix))]
            StreamMode::Pty => unreachable!("effective_stream degrades pty off-unix"),
        };
        if let Some(receipt) = receipt {
            if let Ok(text) = std::fs::read_to_string(&receipt) {
                if let Ok(payload) = serde_json::from_str::<Value>(&text) {
                    if let Some(id) = receipt_session_id(&payload) {
                        let _ = self.observed.set(id);
                    }
                    // `SessionIdAndUsage` receipts can carry the turn's
                    // token counters — the same aliases a stream `usage`
                    // object would use; absent fields stay absent.
                    if self.lane.binding_receipt_shape == BindingReceiptShape::SessionIdAndUsage {
                        if let Ok(done) = outcome.as_mut() {
                            if done.usage.is_none() {
                                done.usage = receipt_usage(&payload);
                            }
                        }
                    }
                }
            }
            let _ = std::fs::remove_file(&receipt);
        }
        outcome
    }

    async fn run_stdio(
        &self,
        mut command: tokio::process::Command,
        stdin_payload: Option<String>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        command
            .stdin(if stdin_payload.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = SupervisedChild::spawn(
            &mut command,
            IoPlan {
                stdin: StdioKind::Preserve,
                stdout: StdioKind::Preserve,
                stderr: StdioKind::Preserve,
            },
        )?;
        if let Some(payload) = stdin_payload {
            if let Some(mut stdin) = child.stdin() {
                let bytes = payload.as_bytes();
                if let Some(observer) = self.observer.as_ref() {
                    observer.record("cli.stdin", Direction::Sent, bytes);
                }
                // An undelivered prompt must fail the turn — a quiet drop
                // would let the turn report success for work never asked.
                stdin
                    .write_all(bytes)
                    .await
                    .map_err(|e| AdapterError::io("agent_prompt_write_failed", "cli/stdin", e))?;
            }
        }
        let stdout = child
            .stdout()
            .ok_or_else(|| AdapterError::permanent("agent_pipe_unavailable", "cli/stdio"))?;
        self.collect(child, Box::new(stdout), events).await
    }

    #[cfg(unix)]
    async fn run_pty(
        &self,
        command: tokio::process::Command,
        stdin_payload: Option<String>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        let (child, master) = crate::transport::pty::spawn(command)?;
        let master = master.with_observer(self.observer.clone());
        if let Some(payload) = stdin_payload {
            master
                .write(payload.as_bytes())
                .await
                .map_err(|e| AdapterError::io("agent_prompt_write_failed", "cli/pty", e))?;
        }
        self.collect(child, Box::new(master), events).await
    }

    /// Drain the output stream, emitting chunks, until EOF. There is no
    /// deadline: output EOF is not an exit signal — the process owns its
    /// cleanup and the real exit status is awaited.
    async fn collect(
        &self,
        mut child: SupervisedChild,
        mut reader: Box<dyn ChunkRead>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        if let Some(stderr) = child.stderr() {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        // Turn accumulators: retained text (fallback), NDJSON carry,
        // last-seen usage, and the authoritative result-record text.
        let mut state = TurnCollect::default();
        // Undecoded carry: a multi-byte scalar split across read boundaries
        // stays buffered until the rest of it arrives — decoded output must
        // be the agent's bytes verbatim, never U+FFFD-injected.
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; bounds::READ_CHUNK];
        loop {
            tokio::select! {
                count = reader.read(&mut buffer) => {
                    match count {
                        Ok(0) => break,
                        Ok(count) => {
                            pending.extend_from_slice(&buffer[..count]);
                            loop {
                                match std::str::from_utf8(&pending) {
                                    Ok(_) => {
                                        let text = std::mem::take(&mut pending);
                                        // Checked above: the whole buffer is valid.
                                        let text =
                                            String::from_utf8(text).unwrap_or_default();
                                        self.process_text(&text, &mut state, &events);
                                        break;
                                    }
                                    Err(error) => {
                                        let valid = error.valid_up_to();
                                        let mut text =
                                            String::from_utf8(pending[..valid].to_vec())
                                                .unwrap_or_default();
                                        match error.error_len() {
                                            // Incomplete tail: keep it buffered
                                            // for the next read.
                                            None => {
                                                pending.drain(..valid);
                                                self.process_text(&text, &mut state, &events);
                                                break;
                                            }
                                            Some(bad) => {
                                                text.push('\u{FFFD}');
                                                pending.drain(..valid + bad);
                                                self.process_text(&text, &mut state, &events);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            // A failed read is not EOF — the turn is broken and the
                            // caller must see it, not a quiet half-answer.
                            let _ = child.terminate().await;
                            return Err(AdapterError::io(
                                "agent_output_read_failed",
                                "cli/stdout",
                                error,
                            ));
                        }
                    }
                }
                _ = self.cancelled.notified() => {
                    let _ = child.terminate().await;
                    return Ok(TurnOutcome {
                        stop_reason: "cancelled".to_owned(),
                        text: state.output,
                        truncated: state.truncated,
                        exit_code: None,
                        usage: state.usage,
                    });
                }
            }
        }
        if !pending.is_empty() {
            let tail = String::from_utf8_lossy(&pending).into_owned();
            self.process_text(&tail, &mut state, &events);
        }
        if self.lane.json_lines && !state.lines.is_empty() {
            let tail = std::mem::take(&mut state.lines);
            self.absorb_line(tail.trim_end_matches('\r'), &events, &mut state);
        }
        let exit = child
            .wait_natural(None)
            .await
            .map_err(|e| AdapterError::io("agent_process_wait_failed", "process/wait", e))?;
        Ok(TurnOutcome {
            stop_reason: if exit.and_then(|s| s.code()).unwrap_or(0) == 0 {
                "end_turn"
            } else {
                "error"
            }
            .to_owned(),
            text: state.result_text.unwrap_or(state.output),
            truncated: state.truncated,
            exit_code: exit.and_then(|status| status.code()),
            usage: state.usage,
        })
    }

    /// One decoded span: cap-retained text plus the chunk event — a
    /// `jsonLines` stream instead reports each record as `Raw` and never
    /// pastes protocol bytes into message chunks.
    fn process_text(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if self.lane.json_lines {
            self.retain_text(text, state, events);
            self.absorb_lines(text, state, events);
        } else {
            self.emit_text(text, state, events);
        }
    }

    /// Split complete lines out of the carry and absorb each.
    fn absorb_lines(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        state.lines.push_str(text);
        while let Some(end) = state.lines.find('\n') {
            let line = state.lines[..end].trim_end_matches('\r').to_owned();
            state.lines.drain(..=end);
            self.absorb_line(&line, events, state);
        }
    }

    /// One NDJSON record: arrives as `AgentEvent::Raw`, and the declared
    /// session-id, usage and result-text paths are extracted from it.
    fn absorb_line(
        &self,
        line: &str,
        events: &mpsc::UnboundedSender<AgentEvent>,
        state: &mut TurnCollect,
    ) {
        if line.is_empty() {
            return;
        }
        let Ok(raw) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if self.observed.get().is_none() {
            if let Some(path) = self.lane.session_id_path.as_deref() {
                if let Some(id) = extract_path(&raw, path)
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    let _ = self.observed.set(id.to_owned());
                }
            }
        }
        if let Some(path) = self.lane.usage_path.as_deref() {
            if let Some(found) = extract_path(&raw, path).and_then(token_usage) {
                state.usage = Some(found.clone());
                let _ = events.send(AgentEvent::Usage {
                    usage: found,
                    raw: Some(raw.clone()),
                });
            }
        }
        if let (Some(selector), Some(path)) = (
            self.lane.result_selector.as_deref(),
            self.lane.result_path.as_deref(),
        ) {
            let selector_field = self.lane.result_selector_field.as_deref().unwrap_or("type");
            if raw.get(selector_field).and_then(Value::as_str) == Some(selector) {
                if let Some(text) = extract_path(&raw, path).and_then(Value::as_str) {
                    state.result_text = Some(text.to_owned());
                }
            }
        }
        let _ = events.send(AgentEvent::Raw { raw });
    }

    /// Retain a decoded span under the byte cap and signal `Truncated`
    /// once — the `jsonLines` path, where records are `Raw` events and the
    /// accumulated text is only the outcome's fallback.
    fn retain_text(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let mut accepted = text.len();
        if let Some(limit) = self.max_output_bytes {
            accepted = limit.saturating_sub(state.output.len()).min(text.len());
            while !text.is_char_boundary(accepted) {
                accepted -= 1;
            }
        }
        if accepted > 0 {
            state.output.push_str(&text[..accepted]);
        }
        if accepted < text.len() && !state.truncated {
            state.truncated = true;
            let _ = events.send(AgentEvent::Truncated);
        }
    }

    /// Emit one decoded text span: retain up to the cap, signal `Truncated`
    /// once, and mirror the retained prefix as a chunk event. The byte cap
    /// applies to retained output; `accepted` never splits a scalar.
    fn emit_text(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if text.is_empty() {
            return;
        }
        let mut accepted = text.len();
        if let Some(limit) = self.max_output_bytes {
            accepted = limit.saturating_sub(state.output.len()).min(text.len());
            while !text.is_char_boundary(accepted) {
                accepted -= 1;
            }
        }
        if accepted > 0 {
            state.output.push_str(&text[..accepted]);
            let _ = events.send(AgentEvent::MessageChunk {
                text: text[..accepted].to_string(),
            });
        }
        if accepted < text.len() && !state.truncated {
            state.truncated = true;
            let _ = events.send(AgentEvent::Truncated);
        }
    }
}

/// Everything one CLI turn accumulates while it collects output.
#[derive(Default)]
struct TurnCollect {
    /// Cap-retained stream text — the outcome's fallback when no
    /// result record supplied authoritative text.
    output: String,
    truncated: bool,
    /// NDJSON carry for `jsonLines`: a line split across reads waits for
    /// its newline; an unterminated tail at EOF is still one record.
    lines: String,
    usage: Option<TokenUsage>,
    /// A `result_selector`/`result_path` record's authoritative text.
    result_text: Option<String>,
}

/// Uniform chunk source over a child stdout pipe or a pty master.
trait ChunkRead: Send + Unpin {
    fn read<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + 'a>>;
}

impl ChunkRead for tokio::process::ChildStdout {
    fn read<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + 'a>>
    {
        Box::pin(AsyncReadExt::read(self, buffer))
    }
}

#[cfg(unix)]
impl ChunkRead for crate::transport::pty::PtyMaster {
    fn read<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + 'a>>
    {
        Box::pin(async move {
            let chunk = self.read().await?;
            let count = chunk.len().min(buffer.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            Ok(count)
        })
    }
}

fn effective_stream(requested: StreamMode) -> StreamMode {
    match requested {
        StreamMode::Pty if cfg!(unix) => StreamMode::Pty,
        _ => StreamMode::Stdio,
    }
}

/// Substitute `{prompt}`/`{model}`/`{effort}`/`{instructions}`/
/// `{permission}`/`{sessionId}`/`{cwd}`/`{provider}` placeholders from the
/// shared variable map. An argument that is exactly a placeholder
/// resolving empty is dropped, together with the flag immediately
/// preceding it when that flag exists only to carry the value (`--model
/// "{model}"` with no model must not send an empty string). An argument
/// whose trailing `={placeholder}` or leading `--{placeholder}` resolves
/// empty is dropped too — a dangling `--model=` or bare `--` is not a
/// valid argv token. Returns whether the prompt still needs a stdin write
/// (no placeholder consumed it).
fn substitute_args(args: &[String], vars: &Vars) -> (Vec<String>, bool) {
    let mut used_prompt = false;
    let mut resolved = Vec::with_capacity(args.len());
    for arg in args {
        if arg.contains("{prompt}") {
            used_prompt = true;
        }
        // Empty-valued placeholders that form a flag tail (`--k={x}`) or
        // a flag body (`--{x}`) leave a malformed token: drop the arg.
        let dangling = vars.iter().any(|(key, value)| {
            value.is_empty()
                && (arg.contains(&format!("={{{key}}}")) || *arg == format!("--{{{key}}}"))
        });
        let mut text = arg.clone();
        for (key, value) in vars {
            text = text.replace(&format!("{{{key}}}"), value);
        }
        let bare = arg.len() > 2 && arg.starts_with('{') && arg.ends_with('}');
        if (bare && text.is_empty()) || dangling {
            if resolved
                .last()
                .is_some_and(|flag: &String| flag.starts_with('-'))
            {
                resolved.pop();
            }
            continue;
        }
        resolved.push(text);
    }
    (resolved, !used_prompt)
}

/// Session identity inside a binding-file receipt: the hook contract's
/// `conversationId`-family keys, or the same set nested inside a JSON
/// `hookPayload` string.
fn receipt_session_id(payload: &Value) -> Option<String> {
    let keys = [
        "conversationId",
        "conversation_id",
        "sessionId",
        "session_id",
    ];
    let read = |value: &Value| {
        keys.iter()
            .find_map(|key| value.get(*key)?.as_str().map(str::to_owned))
            .filter(|id| !id.is_empty())
    };
    read(payload).or_else(|| {
        let nested: Value = serde_json::from_str(payload.get("hookPayload")?.as_str()?).ok()?;
        read(&nested)
    })
}

/// Token counters a receipt may carry under `usage`/`tokenUsage`, top
/// level or inside a nested JSON `hookPayload` string — the same field
/// aliases a stream usage object uses.
fn receipt_usage(payload: &Value) -> Option<TokenUsage> {
    let find = |value: &Value| {
        ["usage", "tokenUsage", "token_usage"]
            .iter()
            .find_map(|key| value.get(*key))
            .and_then(token_usage)
    };
    find(payload).or_else(|| {
        let nested: Value = serde_json::from_str(payload.get("hookPayload")?.as_str()?).ok()?;
        find(&nested)
    })
}

/// Extract token counters from a usage object. Field aliases cover the
/// vendor spellings (`input_tokens`, `promptTokens`, `cache_read…`); an
/// absent counter stays absent — the adapter never invents a zero.
fn token_usage(value: &Value) -> Option<TokenUsage> {
    let object = value.as_object()?;
    let pick = |names: &[&str]| names.iter().find_map(|name| object.get(*name)?.as_u64());
    let usage = TokenUsage {
        input: pick(&[
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
        ]),
        cached_input: pick(&[
            "cached_input_tokens",
            "cachedInputTokens",
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cache_read_tokens",
        ]),
        output: pick(&[
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
        ]),
        total: pick(&["total_tokens", "totalTokens"]),
    };
    (usage.input.is_some()
        || usage.cached_input.is_some()
        || usage.output.is_some()
        || usage.total.is_some())
    .then_some(usage)
}

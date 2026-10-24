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

use std::io;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{
    token_usage, AgentEvent, BindingReceiptShape, CliLaneSpec, CommandSpec, PromptChannel,
    SessionRequest, StreamMode, TokenUsage, TurnOutcome,
};
use crate::observe::{Direction, SharedObserver};
use crate::supervision::{IoPlan, StdioKind, SupervisedChild};
use crate::template::{
    contains_placeholder, contains_placeholder_text, extract_path, substitute, substitute_text,
    vars, VarSpec, Vars,
};

pub struct CliLane {
    pub command: CommandSpec,
    pub lane: CliLaneSpec,
    pub secrets: std::sync::Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
    pub max_output_bytes: Option<usize>,
    /// Fired by `AgentSession::cancel`; the in-flight turn's process tree is
    /// terminated. Explicit user cancellation only — transport failures and
    /// observer disconnects never reach this.
    pub cancelled: std::sync::Arc<watch::Sender<()>>,
    /// Session binding captured from a `sessionIdPath` match on stdout —
    /// the first observed value wins and later turns resume through it.
    pub observed: std::sync::Arc<std::sync::OnceLock<String>>,
    /// A turn's process-cleanup failure is recorded here and surfaced by
    /// `AgentSession::close`; an I/O failure's own error stays the turn's
    /// result and never shadows the cleanup failure.
    pub cleanup_failure: std::sync::Mutex<Option<AdapterError>>,
}

impl CliLane {
    fn record_cleanup_failure(&self, error: AdapterError) {
        *self.cleanup_failure.lock().unwrap() = Some(error);
    }
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
            tokio::spawn(bounds::drain_stderr(
                stderr,
                self.observer.clone(),
                bounds::StderrTail::default(),
            ));
        }
        let Some(mut stdout) = child.stdout() else {
            let _ = child.terminate().await;
            return Err(AdapterError::permanent(
                "agent_pipe_unavailable",
                "cli/stdio",
            ));
        };
        // Only the first non-empty line is a binding. Bound that line and
        // drain everything else without retaining it, including on overflow.
        let limit = self
            .max_output_bytes
            .unwrap_or(crate::framing::DEFAULT_MAX_MESSAGE_BYTES);
        let mut line = Vec::new();
        let mut id = None;
        let mut oversized = false;
        let mut buffer = [0u8; bounds::READ_CHUNK];
        loop {
            let count = match AsyncReadExt::read(&mut stdout, &mut buffer).await {
                Ok(count) => count,
                Err(error) => {
                    let _ = child.terminate().await;
                    return Err(AdapterError::io(
                        "agent_output_read_failed",
                        "cli/stdout",
                        error,
                    ));
                }
            };
            if count == 0 {
                break;
            }
            if id.is_some() || oversized {
                continue;
            }
            for byte in &buffer[..count] {
                if *byte == b'\n' {
                    let candidate = String::from_utf8_lossy(&line).trim().to_owned();
                    line.clear();
                    if !candidate.is_empty() {
                        id = Some(candidate);
                        break;
                    }
                } else if line.len() < limit {
                    line.push(*byte);
                } else {
                    oversized = true;
                    line.clear();
                    break;
                }
            }
        }
        let exit = child
            .wait_natural(None)
            .await
            .map_err(|error| AdapterError::io("agent_process_wait_failed", "cli/open", error))?;
        if oversized {
            return Err(AdapterError::permanent(
                "agent_session_id_too_large",
                "cli/open",
            ));
        }
        if exit.is_some_and(|status| !status.success()) {
            return Err(AdapterError::permanent(
                "agent_session_open_failed",
                "cli/open",
            ));
        }
        if id.is_none() {
            let candidate = String::from_utf8_lossy(&line).trim().to_owned();
            if !candidate.is_empty() {
                id = Some(candidate);
            }
        }
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
    /// arrive. Output retention follows the configured budget; protocol
    /// records use a separate bounded parsing buffer.
    pub async fn run_turn(
        &self,
        request: &SessionRequest,
        prompt: &str,
        model: Option<&str>,
        effort: Option<&str>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        // Each active turn observes cancellation even between I/O awaits.
        // A subscription starts at the current version, so idle cancellation
        // never carries over to a later turn.
        let cancelled = self.cancelled.subscribe();
        self.run_turn_subscribed(request, prompt, model, effort, events, cancelled)
            .await
    }

    /// The facade subscribes before its shared-close check, so closing cannot
    /// fall into a gap between admission and cancellation subscription.
    pub(crate) async fn run_turn_subscribed(
        &self,
        request: &SessionRequest,
        prompt: &str,
        model: Option<&str>,
        effort: Option<&str>,
        events: mpsc::UnboundedSender<AgentEvent>,
        cancelled: watch::Receiver<()>,
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
        let resume_expressible = self.lane.capabilities().session_resume;
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
        // A resume-shaped argv prefix (a subcommand that reorders argv,
        // `codex exec resume <id>`) replaces the spawn args wholesale;
        // in that shape `resume_args` is the complete arg template —
        // flags valid only at session creation (`-C`, `-s`) do not
        // reappear. Without a spawn prefix `resume_args` appends.
        let resume_spawn = binding.is_some() && !self.lane.resume_spawn_args.is_empty();
        let mut args = if resume_spawn {
            self.lane.resume_args.clone()
        } else {
            self.lane.args.clone()
        };
        if !resume_spawn && binding.is_some() && !self.lane.resume_args.is_empty() {
            args.extend(self.lane.resume_args.iter().cloned());
        }
        let model = model.or(request.config.model.as_deref());
        let effort = effort.or(request.config.effort.as_deref());
        // Lanes that encode effort inside the model id rewrite the tier
        // segment — `effort` has no argument of its own on that wire, so
        // the folded value travels through `{model}` alone.
        let effort_carrier = || {
            self.lane
                .args
                .iter()
                .any(|argument| contains_placeholder_text(argument, "effort"))
                || self
                    .lane
                    .stdin_template
                    .as_ref()
                    .is_some_and(|template| contains_placeholder(template, "effort"))
        };
        let folded_model = match (model, effort) {
            (Some(base), Some(level))
                if !self.lane.effort_model_suffixes.is_empty() && !level.trim().is_empty() =>
            {
                Some(effort_suffixed_model(
                    base,
                    level,
                    &self.lane.effort_model_suffixes,
                ))
            }
            (None, Some(level))
                if !self.lane.effort_model_suffixes.is_empty()
                    && !level.trim().is_empty()
                    && !effort_carrier() =>
            {
                // Without a model there is nothing the tier can rewrite,
                // and this lane has no direct effort argument either.
                return Err(AdapterError::permanent(
                    "agent_effort_unsupported",
                    "cli/args",
                ));
            }
            _ => None,
        };
        let (model, effort) = match folded_model.as_deref() {
            Some(folded) => (Some(folded), None),
            None => (model, effort),
        };
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
        if resume_spawn {
            spec.args = substitute_args(&self.lane.resume_spawn_args, &turn_vars).0;
        }
        // The command's own args are the fixed spawn prefix (subcommands,
        // wrapper flags); the lane's resolved template follows them.
        spec.args.extend(args);
        let mut env = crate::env::resolve(&spec.env, &request.env, &*self.secrets).await?;
        // A `binding_file_env` lane mints the scratch receipt path the
        // agent's installed hook writes this turn's session identity to.
        let receipt = self
            .lane
            .binding_file_env
            .as_ref()
            .map(|name| {
                let mut builder = tempfile::Builder::new();
                builder.prefix("a4aa-receipt-");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    builder.permissions(std::fs::Permissions::from_mode(0o700));
                }
                let directory = builder.tempdir().map_err(|error| {
                    AdapterError::io("agent_receipt_directory_failed", "cli/receipt", error)
                })?;
                let path = directory.path().join("receipt.json");
                env.insert(name.clone(), path.as_os_str().to_os_string());
                Ok::<_, AdapterError>((directory, path))
            })
            .transpose()?;
        let command = SupervisedChild::command(&spec, &env, request.cwd.as_deref());

        let mut outcome = match effective_stream(self.lane.stream) {
            StreamMode::Stdio => {
                self.run_stdio(command, stdin_payload, events, cancelled)
                    .await
            }
            #[cfg(unix)]
            StreamMode::Pty => {
                self.run_pty(command, stdin_payload, events, cancelled)
                    .await
            }
            #[cfg(not(unix))]
            StreamMode::Pty => unreachable!("effective_stream degrades pty off-unix"),
        };
        if let Some((directory, receipt)) = receipt {
            let limit = self
                .max_output_bytes
                .unwrap_or(crate::framing::DEFAULT_MAX_MESSAGE_BYTES)
                .max(crate::framing::DEFAULT_MAX_MESSAGE_BYTES);
            let metadata = match read_receipt(&receipt, self.lane.binding_receipt_shape, limit) {
                Ok(metadata) => metadata,
                Err(mut error) => {
                    // Keep failed receipt evidence; the external turn may have
                    // completed, so this error must never invite whole-turn replay.
                    let mut details = error.detail.take().into_iter().collect::<Vec<_>>();
                    if let Err(turn_error) = &outcome {
                        details.push(format!("turn error: {turn_error}"));
                    } else if let Ok(done) = &outcome {
                        if done.stop_reason != "end_turn" {
                            details.push(format!("turn stop reason: {}", done.stop_reason));
                        }
                    }
                    details.push(format!("receipt evidence: {}", directory.keep().display()));
                    error.detail = Some(details.join("; "));
                    return Err(error);
                }
            };
            if let Some(metadata) = metadata {
                let _ = self.observed.set(metadata.session_id);
                if let Ok(done) = outcome.as_mut() {
                    if done.usage.is_none() {
                        done.usage = metadata.usage;
                    }
                }
            }
            if let Err(error) = &mut outcome {
                let evidence = format!("receipt evidence: {}", directory.keep().display());
                error.detail = Some(match error.detail.take() {
                    Some(detail) => format!("{detail}; {evidence}"),
                    None => evidence,
                });
            } else {
                directory
                    .close()
                    .map_err(|_| receipt_error("agent_receipt_cleanup_failed"))?;
            }
        }
        outcome
    }

    async fn run_stdio(
        &self,
        mut command: tokio::process::Command,
        stdin_payload: Option<String>,
        events: mpsc::UnboundedSender<AgentEvent>,
        mut cancelled: watch::Receiver<()>,
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
        let stderr_tail = bounds::StderrTail::default();
        let stderr_drain = child
            .stderr()
            .map(|stderr| bounds::drain_stderr(stderr, self.observer.clone(), stderr_tail.clone()));
        let stdin = child.stdin();
        let stdout = child
            .stdout()
            .ok_or_else(|| AdapterError::permanent("agent_pipe_unavailable", "cli/stdio"))?;
        let output_cancelled = cancelled.clone();
        let write = async {
            if let Some(payload) = stdin_payload {
                let mut stdin = stdin.ok_or_else(|| {
                    AdapterError::permanent("agent_pipe_unavailable", "cli/stdin")
                })?;
                if let Some(observer) = self.observer.as_ref() {
                    observer.record("cli.stdin", Direction::Sent, payload.as_bytes());
                }
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => {}
                    result = stdin.write_all(payload.as_bytes()) => {
                        match result {
                            // The child already closed stdin — it exited
                            // (or stopped listening) before the prompt
                            // arrived. The prompt is undeliverable, and the
                            // child's own exit outcome is the authoritative
                            // verdict for this turn; keep collecting so the
                            // real exit classification surfaces.
                            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
                            result => result.map_err(|error| {
                                AdapterError::io("agent_prompt_write_failed", "cli/stdin", error)
                            })?,
                        }
                    }
                }
            }
            Ok::<(), AdapterError>(())
        };
        // The prompt write rides inside `collect`'s select so a write
        // failure keeps the child owned: the driver terminates and awaits
        // it explicitly instead of relying on `Drop`'s best effort.
        self.collect(
            child,
            Box::new(stdout),
            events,
            output_cancelled,
            write,
            stderr_tail,
            stderr_drain,
        )
        .await
    }

    #[cfg(unix)]
    async fn run_pty(
        &self,
        command: tokio::process::Command,
        stdin_payload: Option<String>,
        events: mpsc::UnboundedSender<AgentEvent>,
        mut cancelled: watch::Receiver<()>,
    ) -> Result<TurnOutcome, AdapterError> {
        let (mut child, master) = crate::transport::pty::spawn(command)?;
        let stderr_tail = bounds::StderrTail::default();
        let stderr_drain = child
            .stderr()
            .map(|stderr| bounds::drain_stderr(stderr, self.observer.clone(), stderr_tail.clone()));
        let master = master.with_observer(self.observer.clone());
        let writer = master.writer();
        let output_cancelled = cancelled.clone();
        let write = async {
            if let Some(payload) = stdin_payload {
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => {}
                    result = writer.write(payload.as_bytes()) => {
                        match result {
                            // Same verdict as the stdio write: a dead slave
                            // makes the master write fail with EIO — keep
                            // collecting for the child's real exit outcome.
                            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
                            result => {
                                result.map_err(|error| {
                                    AdapterError::io(
                                        "agent_prompt_write_failed",
                                        "cli/pty",
                                        error,
                                    )
                                })?;
                            }
                        }
                    }
                }
            }
            Ok::<(), AdapterError>(())
        };
        // Same ownership as run_stdio: the write rides inside `collect`.
        self.collect(
            child,
            Box::new(master),
            events,
            output_cancelled,
            write,
            stderr_tail,
            stderr_drain,
        )
        .await
    }

    /// Drain the output stream, emitting chunks, until EOF. There is no
    /// deadline: output EOF is not an exit signal — the process owns its
    /// cleanup and the real exit status is awaited. The prompt `write`
    /// future rides the same select; a write or read failure still
    /// terminates and reaps the child explicitly before the original error
    /// returns, and a cleanup failure is recorded for `close`.
    #[allow(clippy::too_many_arguments)]
    async fn collect<W: std::future::Future<Output = Result<(), AdapterError>>>(
        &self,
        mut child: SupervisedChild,
        mut reader: Box<dyn ChunkRead>,
        events: mpsc::UnboundedSender<AgentEvent>,
        mut cancelled: watch::Receiver<()>,
        write: W,
        stderr_tail: bounds::StderrTail,
        stderr_drain: Option<tokio::task::JoinHandle<io::Result<()>>>,
    ) -> Result<TurnOutcome, AdapterError> {
        // Turn accumulators: retained text (fallback), NDJSON carry,
        // last-seen usage, and the authoritative result-record text.
        let mut state = TurnCollect::default();
        // Undecoded carry: a multi-byte scalar split across read boundaries
        // stays buffered until the rest of it arrives — decoded output must
        // be the agent's bytes verbatim, never U+FFFD-injected.
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; bounds::READ_CHUNK];
        let mut write = std::pin::pin!(write);
        let mut write_done = false;
        loop {
            tokio::select! {
                biased;
                _ = cancelled.changed() => {
                    child.terminate().await?;
                    return Ok(cancelled_outcome(state));
                }
                result = &mut write, if !write_done => {
                    write_done = true;
                    if let Err(error) = result {
                        // The prompt never fully reached the agent: the turn
                        // is broken, but the process stays owned — terminate
                        // and await the reap before the error returns.
                        if let Err(cleanup) = child.terminate().await {
                            self.record_cleanup_failure(cleanup);
                        }
                        return Err(error);
                    }
                }
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
                            // caller must see it, not a quiet half-answer. Same
                            // ownership: terminate and await the reap, and record
                            // a cleanup failure rather than shadowing the read
                            // error with it.
                            if let Err(cleanup) = child.terminate().await {
                                self.record_cleanup_failure(cleanup);
                            }
                            return Err(AdapterError::io(
                                "agent_output_read_failed",
                                "cli/stdout",
                                error,
                            ));
                        }
                    }
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
        let exit = tokio::select! {
            biased;
            _ = cancelled.changed() => {
                child.terminate().await?;
                return Ok(cancelled_outcome(state));
            }
            result = child.wait_natural(None) => {
                result.map_err(|e| AdapterError::io("agent_process_wait_failed", "process/wait", e))?
            }
        };
        let succeeded = exit.is_some_and(|status| status.success());
        if !succeeded {
            // The child's own error text names quota, capacity and model
            // refusals a flattened "error" stop reason would lose: surface
            // them as a typed failure so custody can fail over to another
            // candidate instead of retrying in place. Exit can race the
            // drain task — join it first so the final refusal line is in
            // the snapshot, not still in flight.
            if let Some(drain) = stderr_drain {
                let _ = drain.await;
            }
            let tail = stderr_tail.text();
            let haystack = format!("{tail}\n{}", state.output);
            if crate::error::quota_or_capacity_refusal(&haystack) {
                let detail = tail
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(str::trim)
                    .unwrap_or("child exited non-zero");
                // Text matching cannot prove the turn had no billable
                // effect — partial work may already have run — so the
                // failover-eligible refusal stays uncertain-effect.
                return Err(AdapterError::uncertain("agent_quota_exhausted", "cli/turn")
                    .with_detail(detail.to_string()));
            }
        }
        Ok(TurnOutcome {
            // `ExitStatus::code()` is `None` when a signal killed the child;
            // success is judged by `success()`, never by a missing code.
            stop_reason: if succeeded { "end_turn" } else { "error" }.to_owned(),
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
        self.retain_text(text, state, events);
        if self.lane.json_lines {
            self.absorb_lines(text, state, events);
        }
    }

    /// Split complete lines out of the carry and absorb each.
    fn absorb_lines(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        // Like framing::read_frame and Tokio's LinesCodec, inspect only the
        // new chunk and drain an oversized record through its terminator.
        // Keep enough scratch space for small control records even when the
        // caller's retained-output budget is very small.
        let frame_limit = self
            .max_output_bytes
            .map(|limit| limit.max(crate::framing::DEFAULT_MAX_MESSAGE_BYTES));
        for part in text.split_inclusive('\n') {
            let complete = part.ends_with('\n');
            let content = part.strip_suffix('\n').unwrap_or(part);
            if !state.discarding_line {
                if frame_limit
                    .is_some_and(|limit| state.lines.len().saturating_add(content.len()) > limit)
                {
                    state.lines.clear();
                    state.discarding_line = true;
                    signal_truncated(state, events);
                } else {
                    state.lines.push_str(content);
                }
            }
            if complete {
                if !state.discarding_line {
                    let line = std::mem::take(&mut state.lines);
                    self.absorb_line(line.trim_end_matches('\r'), events, state);
                }
                state.discarding_line = false;
            }
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
        let emit_raw = self
            .max_output_bytes
            .is_none_or(|limit| line.len() <= limit.saturating_sub(state.raw_bytes));
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
                if emit_raw {
                    let _ = events.send(AgentEvent::Usage {
                        usage: found,
                        raw: None,
                    });
                }
            }
        }
        if let (Some(selector), Some(path)) = (
            self.lane.result_selector.as_deref(),
            self.lane.result_path.as_deref(),
        ) {
            let selector_field = self.lane.result_selector_field.as_deref().unwrap_or("type");
            if raw.get(selector_field).and_then(Value::as_str) == Some(selector) {
                if let Some(text) = extract_path(&raw, path).and_then(Value::as_str) {
                    let retained = bounds::bounded_prefix(text, self.max_output_bytes);
                    state.result_text = Some(retained.to_owned());
                    if retained.len() < text.len() {
                        signal_truncated(state, events);
                    }
                }
            }
        }
        if emit_raw {
            state.raw_bytes = state.raw_bytes.saturating_add(line.len());
            let _ = events.send(AgentEvent::Raw { raw });
        } else {
            signal_truncated(state, events);
        }
    }

    /// Retain one UTF-8 prefix under the shared byte budget. Plain streams
    /// mirror it as a message chunk; structured streams emit Raw separately.
    /// Both paths signal truncation exactly once.
    fn retain_text(
        &self,
        text: &str,
        state: &mut TurnCollect,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if state.output_truncated {
            return;
        }
        let remaining = self
            .max_output_bytes
            .map(|limit| limit.saturating_sub(state.output.len()));
        let retained = bounds::bounded_prefix(text, remaining);
        state.output.push_str(retained);
        if !self.lane.json_lines && !retained.is_empty() {
            let _ = events.send(AgentEvent::MessageChunk {
                text: retained.to_owned(),
            });
        }
        if retained.len() < text.len() {
            state.output_truncated = true;
            signal_truncated(state, events);
        }
    }
}

/// Everything one CLI turn accumulates while it collects output.
#[derive(Default)]
struct TurnCollect {
    /// Cap-retained stream text — the outcome's fallback when no
    /// result record supplied authoritative text.
    output: String,
    output_truncated: bool,
    truncated: bool,
    /// NDJSON carry for `jsonLines`: a line split across reads waits for
    /// its newline; an unterminated tail at EOF is still one record.
    lines: String,
    discarding_line: bool,
    raw_bytes: usize,
    usage: Option<TokenUsage>,
    /// A `result_selector`/`result_path` record's authoritative text.
    result_text: Option<String>,
}

fn signal_truncated(state: &mut TurnCollect, events: &mpsc::UnboundedSender<AgentEvent>) {
    if !state.truncated {
        state.truncated = true;
        let _ = events.send(AgentEvent::Truncated);
    }
}

fn cancelled_outcome(state: TurnCollect) -> TurnOutcome {
    TurnOutcome {
        stop_reason: "cancelled".to_owned(),
        text: state.result_text.unwrap_or(state.output),
        truncated: state.truncated,
        exit_code: None,
        usage: state.usage,
    }
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
/// shared variable map. An argument that is exactly an absent optional
/// selector is dropped, together with the value flag immediately
/// preceding it when that flag exists only to carry the value (`--model
/// "{model}"` with no model must not send an empty string). An argument
/// whose trailing `={placeholder}` or leading `--{placeholder}` resolves
/// empty is dropped too — a dangling `--model=` or bare `--` is not a
/// valid argv token. Returns whether the prompt still needs a stdin write
/// (no placeholder consumed it).
fn substitute_args(args: &[String], vars: &Vars) -> (Vec<String>, bool) {
    let mut used_prompt = false;
    let mut resolved = Vec::with_capacity(args.len());
    let mut previous_literal_flag = false;
    for arg in args {
        if arg.contains("{prompt}") {
            used_prompt = true;
        }
        // Empty-valued placeholders that form a flag tail (`--k={x}`) or
        // a flag body (`--{x}`) leave a malformed token: drop the arg.
        let dangling = vars
            .absent_names()
            .any(|key| arg.ends_with(&format!("={{{key}}}")) || *arg == format!("--{{{key}}}"));
        let text = substitute_text(arg, vars);
        if vars.is_absent_option(arg) {
            if previous_literal_flag {
                resolved.pop();
            }
            previous_literal_flag = false;
            continue;
        }
        if dangling {
            previous_literal_flag = false;
            continue;
        }
        previous_literal_flag = arg.starts_with('-') && !arg.contains('{');
        resolved.push(text);
    }
    (resolved, !used_prompt)
}

struct ReceiptMetadata {
    session_id: String,
    usage: Option<TokenUsage>,
}

fn receipt_error(code: &'static str) -> AdapterError {
    let mut error = AdapterError::uncertain(code, "cli/receipt");
    error.retryable = false;
    error
}

/// A model id carrying effort as a `-<level>` suffix gets the requested
/// level rewritten into that segment (`cursor-grok-4.6-high` + `low` →
/// `cursor-grok-4.6-low`); a trailing `-fast` marker is preserved
/// (`x-high-fast` → `x-low-fast`). Models with no known tier segment
/// gain the level as a new suffix (`gpt-5.2` + `high` → `gpt-5.2-high`).
fn effort_suffixed_model(model: &str, effort: &str, tiers: &[String]) -> String {
    let (stem, fast) = match model.strip_suffix("-fast") {
        Some(stem) => (stem, "-fast"),
        None => (model, ""),
    };
    // Longest declared tier wins: `extra-high` must beat `high` so
    // `gpt-5.5-extra-high` rewrites to `gpt-5.5-low`, never `gpt-5.5-extra-low`.
    let mut matched = 0;
    for tier in tiers {
        if tier.len() > matched
            && stem.len() > tier.len()
            && stem.ends_with(tier.as_str())
            && stem.as_bytes()[stem.len() - tier.len() - 1] == b'-'
        {
            matched = tier.len();
        }
    }
    let strip = if matched == 0 { 0 } else { matched + 1 };
    let family = &stem[..stem.len() - strip];
    format!("{family}-{effort}{fast}")
}

/// Missing optional receipts are allowed. Existing but unreadable, oversized,
/// malformed, or identity-free receipts are failures and remain on disk.
fn read_receipt(
    path: &std::path::Path,
    shape: BindingReceiptShape,
    limit: usize,
) -> Result<Option<ReceiptMetadata>, AdapterError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(receipt_error("agent_receipt_read_failed")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(receipt_error("agent_receipt_read_failed")),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(receipt_error("agent_receipt_read_failed")),
    };
    if !file
        .metadata()
        .map_err(|_| receipt_error("agent_receipt_read_failed"))?
        .is_file()
    {
        return Err(receipt_error("agent_receipt_read_failed"));
    }
    let mut reader = std::io::Read::take(file, limit.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes)
        .map_err(|_| receipt_error("agent_receipt_read_failed"))?;
    if bytes.len() > limit {
        return Err(receipt_error("agent_receipt_too_large"));
    }
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| receipt_error("agent_receipt_invalid"))?;
    if !payload.is_object() {
        return Err(receipt_error("agent_receipt_invalid"));
    }
    let nested = payload
        .get("hookPayload")
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| receipt_error("agent_receipt_invalid"))?;
            let value: Value =
                serde_json::from_str(text).map_err(|_| receipt_error("agent_receipt_invalid"))?;
            if !value.is_object() {
                return Err(receipt_error("agent_receipt_invalid"));
            }
            Ok(value)
        })
        .transpose()?;
    let sources = [Some(&payload), nested.as_ref()];
    let session_id = sources
        .iter()
        .flatten()
        .find_map(|value| {
            [
                "conversationId",
                "conversation_id",
                "sessionId",
                "session_id",
            ]
            .iter()
            .find_map(|key| {
                value
                    .get(*key)?
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
        })
        .ok_or_else(|| receipt_error("agent_receipt_session_id_missing"))?;
    let usage = if shape == BindingReceiptShape::SessionIdAndUsage {
        sources.iter().flatten().find_map(|value| {
            ["usage", "tokenUsage", "token_usage"]
                .iter()
                .find_map(|key| value.get(*key))
                .and_then(token_usage)
        })
    } else {
        None
    };
    Ok(Some(ReceiptMetadata { session_id, usage }))
}

#[cfg(all(test, unix))]
mod receipt_file_type_tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn receipts_reject_links_and_pipes_without_waiting_for_a_writer() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.json");
        std::fs::write(&valid, r#"{"sessionId":"fixture"}"#).unwrap();
        assert!(read_receipt(&valid, BindingReceiptShape::SessionId, 1024)
            .unwrap()
            .is_some());
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&valid, &link).unwrap();
        assert!(read_receipt(&link, BindingReceiptShape::SessionId, 1024).is_err());
        let pipe = directory.path().join("pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = read_receipt(&pipe, BindingReceiptShape::SessionId, 1024);
            send.send(result.is_err()).unwrap();
        });
        assert!(receive
            .recv_timeout(crate::test_settings::WAIT_BUDGET)
            .expect("receipt read blocked on a special file"));
    }
}

#[cfg(test)]
mod effort_suffix_tests {
    use super::effort_suffixed_model;

    fn tiers() -> Vec<String> {
        [
            "minimal",
            "none",
            "low",
            "medium",
            "high",
            "extra-high",
            "xhigh",
            "max",
        ]
        .iter()
        .map(|level| (*level).to_owned())
        .collect()
    }

    #[test]
    fn rewrites_the_tier_segment() {
        let tiers = tiers();
        assert_eq!(
            effort_suffixed_model("cursor-grok-4.6-high", "low", &tiers),
            "cursor-grok-4.6-low"
        );
        assert_eq!(
            effort_suffixed_model("claude-opus-5-thinking-high", "medium", &tiers),
            "claude-opus-5-thinking-medium"
        );
        assert_eq!(
            effort_suffixed_model("claude-4.6-opus-high", "max", &tiers),
            "claude-4.6-opus-max"
        );
    }

    #[test]
    fn longest_tier_wins_over_nested_candidates() {
        let tiers = tiers();
        // `extra-high` must beat `high`: otherwise `gpt-5.5-extra-high`
        // would rewrite to the nonexistent `gpt-5.5-extra-low`.
        assert_eq!(
            effort_suffixed_model("gpt-5.5-extra-high", "low", &tiers),
            "gpt-5.5-low"
        );
    }

    #[test]
    fn fast_marker_survives_the_rewrite() {
        let tiers = tiers();
        assert_eq!(
            effort_suffixed_model("cursor-grok-4.6-high-fast", "xhigh", &tiers),
            "cursor-grok-4.6-xhigh-fast"
        );
        assert_eq!(
            effort_suffixed_model("composer-2.5-fast", "high", &tiers),
            "composer-2.5-high-fast"
        );
    }

    #[test]
    fn models_without_a_tier_gain_the_suffix() {
        let tiers = tiers();
        assert_eq!(
            effort_suffixed_model("gpt-5.2", "high", &tiers),
            "gpt-5.2-high"
        );
        // A hyphenated name whose last segment is not a tier is appended,
        // never corrupted mid-name.
        assert_eq!(
            effort_suffixed_model("claude-4.5-sonnet-thinking", "high", &tiers),
            "claude-4.5-sonnet-thinking-high"
        );
    }
}

#[cfg(test)]
mod output_prefix_tests {
    use super::*;

    #[test]
    fn later_ascii_cannot_fill_a_cut_multibyte_prefix() {
        let lane = CliLane {
            command: CommandSpec {
                program: "fixture".into(),
                args: vec![],
                cwd: None,
                env: Default::default(),
            },
            lane: CliLaneSpec::default(),
            secrets: std::sync::Arc::new(crate::env::NoSecrets),
            observer: None,
            max_output_bytes: Some(4),
            cancelled: std::sync::Arc::new(watch::channel(()).0),
            observed: std::sync::Arc::new(std::sync::OnceLock::new()),
            cleanup_failure: std::sync::Mutex::new(None),
        };
        let (send, mut receive) = mpsc::unbounded_channel();
        let mut state = TurnCollect::default();
        lane.retain_text("界界", &mut state, &send);
        lane.retain_text("a", &mut state, &send);
        assert_eq!(state.output, "界");
        assert!(state.truncated);
        assert_eq!(
            receive.try_recv().unwrap(),
            AgentEvent::MessageChunk { text: "界".into() }
        );
        assert_eq!(receive.try_recv().unwrap(), AgentEvent::Truncated);
        assert!(receive.try_recv().is_err());
    }
}

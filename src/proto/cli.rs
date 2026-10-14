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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{
    AgentEvent, CliLaneSpec, CommandSpec, PromptChannel, SessionRequest, StreamMode, TurnOutcome,
};
use crate::observe::{Direction, SharedObserver};
use crate::supervision::{IoPlan, StdioKind, SupervisedChild};

pub struct CliLane {
    pub command: CommandSpec,
    pub lane: CliLaneSpec,
    pub secrets: std::sync::Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
    pub max_output_bytes: Option<usize>,
}

impl CliLane {
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
        let (args, prompt_via_stdin) = substitute_args(&self.lane.args, prompt, model, effort);
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
        let mut spec = self.command.clone();
        spec.args = args;
        let env = crate::env::resolve(&spec.env, &request.env, &*self.secrets).await?;
        let command = SupervisedChild::command(&spec, &env, request.cwd.as_deref());

        match effective_stream(self.lane.stream) {
            StreamMode::Stdio => self.run_stdio(command, prompt, write_stdin, events).await,
            #[cfg(unix)]
            StreamMode::Pty => self.run_pty(command, prompt, write_stdin, events).await,
            #[cfg(not(unix))]
            StreamMode::Pty => unreachable!("effective_stream degrades pty off-unix"),
        }
    }

    async fn run_stdio(
        &self,
        mut command: tokio::process::Command,
        prompt: &str,
        write_stdin: bool,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        command
            .stdin(if write_stdin {
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
        if write_stdin {
            if let Some(mut stdin) = child.stdin() {
                let mut payload = prompt.as_bytes().to_vec();
                if !prompt.ends_with('\n') {
                    payload.push(b'\n');
                }
                if let Some(observer) = self.observer.as_ref() {
                    observer.record("cli.stdin", Direction::Sent, &payload);
                }
                // An undelivered prompt must fail the turn — a quiet drop
                // would let the turn report success for work never asked.
                stdin
                    .write_all(&payload)
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
        prompt: &str,
        write_stdin: bool,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<TurnOutcome, AdapterError> {
        let (child, master) = crate::transport::pty::spawn(command)?;
        let master = master.with_observer(self.observer.clone());
        if write_stdin {
            let mut payload = prompt.as_bytes().to_vec();
            if !prompt.ends_with('\n') {
                payload.push(b'\n');
            }
            master
                .write(&payload)
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
        let mut output = String::new();
        let mut truncated = false;
        // Undecoded carry: a multi-byte scalar split across read boundaries
        // stays buffered until the rest of it arrives — decoded output must
        // be the agent's bytes verbatim, never U+FFFD-injected.
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; bounds::READ_CHUNK];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    pending.extend_from_slice(&buffer[..count]);
                    loop {
                        match std::str::from_utf8(&pending) {
                            Ok(_) => {
                                let text = std::mem::take(&mut pending);
                                // Checked above: the whole buffer is valid.
                                let text = String::from_utf8(text).unwrap_or_default();
                                self.emit_text(&text, &mut output, &mut truncated, &events);
                                break;
                            }
                            Err(error) => {
                                let valid = error.valid_up_to();
                                let mut text = String::from_utf8(pending[..valid].to_vec())
                                    .unwrap_or_default();
                                match error.error_len() {
                                    // Incomplete tail: keep it buffered
                                    // for the next read.
                                    None => {
                                        pending.drain(..valid);
                                        self.emit_text(&text, &mut output, &mut truncated, &events);
                                        break;
                                    }
                                    Some(bad) => {
                                        text.push('\u{FFFD}');
                                        pending.drain(..valid + bad);
                                        self.emit_text(&text, &mut output, &mut truncated, &events);
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
        if !pending.is_empty() {
            let tail = String::from_utf8_lossy(&pending).into_owned();
            self.emit_text(&tail, &mut output, &mut truncated, &events);
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
            text: output,
            truncated,
            exit_code: exit.and_then(|status| status.code()),
        })
    }

    /// Emit one decoded text span: retain up to the cap, signal `Truncated`
    /// once, and mirror the retained prefix as a chunk event. The byte cap
    /// applies to retained output; `accepted` never splits a scalar.
    fn emit_text(
        &self,
        text: &str,
        output: &mut String,
        truncated: &mut bool,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if text.is_empty() {
            return;
        }
        let mut accepted = text.len();
        if let Some(limit) = self.max_output_bytes {
            accepted = limit.saturating_sub(output.len()).min(text.len());
            while !text.is_char_boundary(accepted) {
                accepted -= 1;
            }
        }
        if accepted > 0 {
            output.push_str(&text[..accepted]);
            let _ = events.send(AgentEvent::MessageChunk {
                text: text[..accepted].to_string(),
            });
        }
        if accepted < text.len() && !*truncated {
            *truncated = true;
            let _ = events.send(AgentEvent::Truncated);
        }
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

/// Substitute `{prompt}`/`{model}`/`{effort}` placeholders. Returns whether
/// the prompt still needs a stdin write (no placeholder consumed it).
fn substitute_args(
    args: &[String],
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> (Vec<String>, bool) {
    let mut used_prompt = false;
    let resolved = args
        .iter()
        .map(|arg| {
            if arg.contains("{prompt}") {
                used_prompt = true;
            }
            arg.replace("{prompt}", prompt)
                .replace("{model}", model.unwrap_or(""))
                .replace("{effort}", effort.unwrap_or(""))
        })
        .collect();
    (resolved, !used_prompt)
}

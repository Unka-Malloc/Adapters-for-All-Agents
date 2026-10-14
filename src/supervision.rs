//! Supervised agent processes.
//!
//! Every agent spawns inside its own process group (Unix) or Job Object
//! (Windows) via `command-group`, so one `terminate` removes the whole tree —
//! including descendants that inherited the agent's pipes. The handle keeps
//! the leader unreaped until teardown so its process-group ID cannot be
//! recycled mid-cleanup.
//!
//! Cancellation distinguishes two intents:
//!
//! - [`SupervisedChild::interrupt`] — user-initiated stop. SIGINT to the
//!   group first so cooperative agents can abort in-flight work, then a
//!   bounded grace, then a tree kill.
//! - [`SupervisedChild::terminate`] — teardown. The tree is killed and the
//!   leader reaped; suitable for completed turns and cleanup paths.

use command_group::{AsyncCommandGroup, AsyncGroupChild};
use std::ffi::OsString;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

use crate::error::AdapterError;

/// Default grace for a graceful exit before the tree is killed.
pub const DEFAULT_INTERRUPT_GRACE: Duration = Duration::from_secs(2);

/// Stdio arrangement for one spawn. `Piped` is required for any stream the
/// lane reads or writes; `Null` keeps counters honest by discarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoPlan {
    pub stdin: StdioKind,
    pub stdout: StdioKind,
    pub stderr: StdioKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioKind {
    Piped,
    Null,
    Inherit,
    /// Leave whatever the caller already configured on the `Command` —
    /// used by transports that attach their own descriptors (pty slave).
    Preserve,
}

impl StdioKind {
    fn apply(self, command: &mut Command, slot: StdioSlot) {
        if let Some(stdio) = self.stdio() {
            match slot {
                StdioSlot::Stdin => {
                    command.stdin(stdio);
                }
                StdioSlot::Stdout => {
                    command.stdout(stdio);
                }
                StdioSlot::Stderr => {
                    command.stderr(stdio);
                }
            }
        }
    }

    fn stdio(self) -> Option<Stdio> {
        match self {
            StdioKind::Piped => Some(Stdio::piped()),
            StdioKind::Null => Some(Stdio::null()),
            StdioKind::Inherit => Some(Stdio::inherit()),
            StdioKind::Preserve => None,
        }
    }
}

enum StdioSlot {
    Stdin,
    Stdout,
    Stderr,
}

impl Default for IoPlan {
    fn default() -> Self {
        Self {
            stdin: StdioKind::Piped,
            stdout: StdioKind::Piped,
            stderr: StdioKind::Piped,
        }
    }
}

/// A spawned agent process group.
pub struct SupervisedChild {
    child: Option<AsyncGroupChild>,
    pid: Option<u32>,
    reaped: bool,
}

impl SupervisedChild {
    /// Spawn `command` in its own process group with the requested pipes.
    ///
    /// The caller is expected to have applied the environment policy already;
    /// this function never touches `env_clear`/`envs` itself.
    pub fn spawn(command: &mut Command, io: IoPlan) -> Result<Self, AdapterError> {
        io.stdin.apply(command, StdioSlot::Stdin);
        io.stdout.apply(command, StdioSlot::Stdout);
        io.stderr.apply(command, StdioSlot::Stderr);
        command.kill_on_drop(false);
        let child = command.group_spawn().map_err(|error| {
            AdapterError::io("agent_process_start_failed", "process/start", error)
        })?;
        let pid = child.id();
        Ok(Self {
            child: Some(child),
            pid,
            reaped: false,
        })
    }

    fn child(&mut self) -> &mut AsyncGroupChild {
        // `child` is `Some` until Drop takes it; every lane path that can
        // reach a `None` has already reported a lifecycle failure.
        self.child.as_mut().expect("supervised child taken")
    }

    /// Build the tokio command for a [`crate::model::CommandSpec`] with a
    /// resolved environment. Does not spawn.
    pub fn command(
        spec: &crate::model::CommandSpec,
        env: &std::collections::BTreeMap<String, OsString>,
        cwd: Option<&std::path::Path>,
    ) -> Command {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args).env_clear().envs(env);
        match cwd.or(spec.cwd.as_deref()) {
            Some(dir) => {
                command.current_dir(dir);
            }
            None => {
                // Never leak the host's cwd into a third-party agent.
                command.current_dir(std::env::temp_dir());
            }
        }
        command
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn stdin(&mut self) -> Option<ChildStdin> {
        self.child().inner().stdin.take()
    }

    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.child().inner().stdout.take()
    }

    pub fn stderr(&mut self) -> Option<ChildStderr> {
        self.child().inner().stderr.take()
    }

    /// Wait for exit; reaps the leader.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child().wait().await;
        if status.is_ok() {
            self.reaped = true;
        }
        status
    }

    /// Poll for a completed exit status without reaping on `None`.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.child().try_wait() {
            Ok(Some(status)) => {
                self.reaped = true;
                Ok(Some(status))
            }
            other => other,
        }
    }

    /// Wait up to `deadline` for natural exit without killing.
    ///
    /// Implemented as `timeout(child.wait())` rather than a `try_wait` poll:
    /// `command-group`'s `try_wait` sweeps `waitpid(-pgid)` and races with
    /// tokio's own reaper — if tokio reaps the leader first, the sweep hits
    /// `ECHILD` and reports `Ok(None)` forever. `wait()` always returns the
    /// leader's real status, and dropping the future on timeout is safe
    /// (the child is not killed, `kill_on_drop` is off).
    pub async fn wait_natural(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return Ok(None);
        }
        match deadline {
            Some(end) => {
                let remaining = end.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, self.child().wait()).await {
                    Ok(status) => {
                        self.reaped = true;
                        status.map(Some)
                    }
                    Err(_) => Ok(None),
                }
            }
            None => self.wait().await.map(Some),
        }
    }

    /// SIGINT the process group, give it `grace`, then kill the tree.
    #[cfg(unix)]
    pub async fn interrupt(&mut self, grace: Duration) -> Result<Option<ExitStatus>, AdapterError> {
        if let Some(status) = self.try_wait().map_err(|error| {
            AdapterError::io("agent_process_wait_failed", "process/interrupt", error)
        })? {
            return Ok(Some(status));
        }
        if let Some(pid) = self.pid {
            // The group leader is deliberately unreaped, so its pgid is still
            // reserved; ESRCH/EPERM mean nothing signalable remains.
            let result = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
            if result != 0 {
                let error = io::Error::last_os_error();
                if !matches!(
                    error.raw_os_error(),
                    Some(code) if code == libc::ESRCH || code == libc::EPERM
                ) {
                    return Err(AdapterError::io(
                        "agent_process_interrupt_failed",
                        "process/interrupt",
                        error,
                    ));
                }
            }
        }
        match self
            .wait_natural(Some(Instant::now() + grace))
            .await
            .map_err(|error| {
                AdapterError::io("agent_process_wait_failed", "process/interrupt", error)
            })? {
            Some(status) => Ok(Some(status)),
            None => self.terminate().await,
        }
    }

    /// Non-Unix fallback: interrupt degrades to terminate.
    #[cfg(not(unix))]
    pub async fn interrupt(
        &mut self,
        _grace: Duration,
    ) -> Result<Option<ExitStatus>, AdapterError> {
        self.terminate().await
    }

    /// Kill the whole process tree and reap the leader.
    pub async fn terminate(&mut self) -> Result<Option<ExitStatus>, AdapterError> {
        if let Some(status) = self.try_wait().map_err(|error| {
            AdapterError::io("agent_process_wait_failed", "process/terminate", error)
        })? {
            return Ok(Some(status));
        }
        if let Err(error) = self.child().start_kill() {
            // The group may already be gone — the leader's exit status can
            // still be retrievable through `wait`.
            if !process_gone(&error) {
                return Err(AdapterError::io(
                    "agent_process_terminate_failed",
                    "process/terminate",
                    error,
                ));
            }
        }
        let status = self.child().wait().await.map_err(|error| {
            AdapterError::io("agent_process_wait_failed", "process/terminate", error)
        })?;
        self.reaped = true;
        Ok(Some(status))
    }
}

/// Whether a failed kill means "nothing signalable remains" rather than a
/// real failure.
#[cfg(unix)]
fn process_gone(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::ESRCH || code == libc::EPERM)
}

/// Non-Unix kill surfaces expose no portable "no such process" errno; a
/// dead process shows up as InvalidInput/NotFound from the platform layer.
#[cfg(not(unix))]
fn process_gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Best-effort teardown: signal the tree now, then let a detached
        // task reap the leader so no zombie survives a dropped handle.
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

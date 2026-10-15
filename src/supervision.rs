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
    exit_status: Option<ExitStatus>,
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
            exit_status: None,
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

    /// Wait for natural leader exit, remove remaining group members, then
    /// reap the leader. On Unix, WNOWAIT reserves its identity until the
    /// group signal has been sent; a recycled PID is never signalled.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        #[cfg(unix)]
        {
            // Register before probing, so an exit between the probe and
            // recv cannot be lost. This is event-driven, without polling.
            let mut changed =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
            while !self.leader_exited()? {
                if changed.recv().await.is_none() {
                    return Err(io::Error::other("child-exit notification channel closed"));
                }
            }
            self.kill_group()?;
        }
        let status = self.child().inner().wait().await?;
        // A Windows Job Object retains its identity after leader exit.
        #[cfg(not(unix))]
        self.kill_group()?;
        self.exit_status = Some(status);
        Ok(status)
    }

    /// Poll for a completed exit status without reaping on `None`.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        #[cfg(unix)]
        {
            if !self.leader_exited()? {
                return Ok(None);
            }
            self.kill_group()?;
        }
        let status = self.child().inner().try_wait()?;
        if status.is_some() {
            #[cfg(not(unix))]
            self.kill_group()?;
            self.exit_status = status;
        }
        Ok(status)
    }

    #[cfg(unix)]
    fn leader_exited(&self) -> io::Result<bool> {
        let pid = self
            .pid
            .ok_or_else(|| io::Error::other("missing child identity"))?;
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.si_signo != 0)
    }

    fn kill_group(&mut self) -> io::Result<()> {
        match self.child().start_kill() {
            #[cfg(target_os = "macos")]
            Err(error)
                if error.raw_os_error() == Some(libc::EPERM)
                    && self.leader_exited()?
                    && self.group_only_has_zombies()? =>
            {
                Ok(())
            }
            Err(error) if !process_gone(&error) => Err(error),
            _ => Ok(()),
        }
    }

    // Darwin returns EPERM for a zombie-only process group. Do not treat
    // arbitrary permission denial as successful termination: the kernel must
    // confirm every remaining member is a zombie while our leader is unreaped.
    #[cfg(target_os = "macos")]
    fn group_only_has_zombies(&self) -> io::Result<bool> {
        let pgid = self
            .pid
            .ok_or_else(|| io::Error::other("missing child identity"))?;
        let mut members = vec![0 as libc::pid_t; 2];
        loop {
            let bytes = members
                .len()
                .checked_mul(std::mem::size_of::<libc::pid_t>())
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| io::Error::other("process-group listing exceeds OS buffer size"))?;
            let count = unsafe {
                libc::proc_listpgrppids(pgid as libc::pid_t, members.as_mut_ptr().cast(), bytes)
            };
            if count <= 0 {
                return Ok(false);
            }
            if count as usize == members.len() {
                members.resize(members.len() * 2, 0);
                continue;
            }
            for pid in &members[..count as usize] {
                let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
                let length = std::mem::size_of_val(&info) as i32;
                let read = unsafe {
                    libc::proc_pidinfo(
                        *pid,
                        libc::PROC_PIDTBSDINFO,
                        1, // PROC_PIDTBSDINFO's arg requests zombie inclusion.
                        (&mut info as *mut libc::proc_bsdinfo).cast(),
                        length,
                    )
                };
                if read == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                    continue; // Reaped after the group listing.
                }
                if read != length || info.pbi_status != libc::SZOMB {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
    }

    /// Wait for natural leader exit, then clean up remaining group members.
    ///
    /// A deadline limits only an explicit interruption grace period. Dropping
    /// this wait before natural exit neither reaps nor kills the leader.
    pub async fn wait_natural(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        match deadline {
            Some(end) => {
                let remaining = end.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, self.wait()).await {
                    Ok(status) => status.map(Some),
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
            // reserved; only ESRCH means nothing signalable remains.
            let result = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
            if result != 0 {
                let error = io::Error::last_os_error();
                if !matches!(
                    error.raw_os_error(),
                    Some(code) if code == libc::ESRCH
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
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        self.kill_group().map_err(|error| {
            AdapterError::io("agent_process_terminate_failed", "process/terminate", error)
        })?;
        let status = self.child().inner().wait().await.map_err(|error| {
            AdapterError::io("agent_process_wait_failed", "process/terminate", error)
        })?;
        self.exit_status = Some(status);
        Ok(Some(status))
    }
}

/// Whether a failed kill means "nothing signalable remains" rather than a
/// real failure.
#[cfg(unix)]
fn process_gone(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| code == libc::ESRCH)
}

/// Non-Unix kill surfaces expose no portable "no such process" errno; a
/// dead process shows up as InvalidInput/NotFound from the platform layer.
#[cfg(not(unix))]
fn process_gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
    )
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self.exit_status.is_some() {
            return;
        }
        // Best-effort teardown: signal the tree now, then let a detached
        // task reap the leader so no zombie survives a dropped handle.
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.inner().wait().await;
                });
            }
        }
    }
}

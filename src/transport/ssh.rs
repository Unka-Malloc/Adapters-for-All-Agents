//! Remote agent spawn over SSH.
//!
//! A remote target names one agent executable on one host; [`SshRemote`]
//! validates that destination and builds a batch-only `ssh` command that runs
//! it under a stated working directory. The local `ssh` client observes the
//! user shell environment (proxy, agent socket, PATH); `SendEnv=-*` keeps
//! host variables from crossing the wire, and every remote value is
//! posix-shell-quoted so no caller-controlled fragment enters the guest
//! shell. The connection carries no credentials and no forwarding — the
//! remote side authenticates through the caller's own SSH agent/keys on the
//! destination.

use std::fmt;
use std::path::Path;
use std::process::Command;

const MAX_HOST_BYTES: usize = 255;
const MAX_USER_BYTES: usize = 255;
const MAX_EXECUTABLE_BYTES: usize = 1024;
const MAX_WORKING_DIRECTORY_BYTES: usize = 4096;
const SSH_CONNECT_TIMEOUT_SECONDS: u8 = 10;

/// One validated remote launch: `ssh <destination> 'cd <wd> && exec <exe> <args>'`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshRemote {
    host: String,
    port: Option<u16>,
    user: Option<String>,
    remote_executable: String,
    working_directory: String,
}

/// Which field of an [`SshRemote`] rejected the declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshRemoteError {
    InvalidHost,
    InvalidPort,
    InvalidUser,
    InvalidExecutable,
    InvalidWorkingDirectory,
}

impl fmt::Display for SshRemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = match self {
            Self::InvalidHost => "host",
            Self::InvalidPort => "port",
            Self::InvalidUser => "user",
            Self::InvalidExecutable => "remote_executable",
            Self::InvalidWorkingDirectory => "working_directory",
        };
        write!(formatter, "ssh remote {field} is invalid")
    }
}

impl std::error::Error for SshRemoteError {}

impl SshRemote {
    /// Validate one remote launch declaration. Every field is bounded and
    /// shaped so it cannot smuggle ssh options (`-oProxyCommand=…`), shell
    /// metacharacters into the remote command, or empty values.
    pub fn new(
        host: &str,
        port: Option<u16>,
        user: Option<&str>,
        remote_executable: &str,
        working_directory: &str,
    ) -> Result<Self, SshRemoteError> {
        if !valid_host(host) {
            return Err(SshRemoteError::InvalidHost);
        }
        if port == Some(0) {
            return Err(SshRemoteError::InvalidPort);
        }
        if let Some(user) = user {
            if !valid_user(user) {
                return Err(SshRemoteError::InvalidUser);
            }
        }
        if !valid_remote_executable(remote_executable) {
            return Err(SshRemoteError::InvalidExecutable);
        }
        if !valid_remote_working_directory(working_directory) {
            return Err(SshRemoteError::InvalidWorkingDirectory);
        }
        Ok(Self {
            host: host.to_string(),
            port,
            user: user.map(str::to_string),
            remote_executable: remote_executable.to_string(),
            working_directory: working_directory.to_string(),
        })
    }

    /// The validated remote executable path.
    pub fn remote_executable(&self) -> &str {
        &self.remote_executable
    }

    /// The validated remote working directory.
    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    /// Build the batch-only `ssh` command that runs `remote_args` on the
    /// remote executable under the working directory.
    ///
    /// Lockdown is fixed: no TTY, no password prompts, strict host-key
    /// checking, every forwarding off, no connection sharing, `SendEnv=-*`
    /// so the client environment never crosses the wire. The client still
    /// observes the user shell environment locally (proxy vars, agent
    /// socket, PATH) because the local `ssh` binary resolves them.
    pub fn command(&self, remote_args: &[&str]) -> Command {
        let destination = match self.user.as_deref() {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        };
        let remote_args = remote_args
            .iter()
            .map(|value| posix_shell_quote(value))
            .collect::<Vec<_>>()
            .join(" ");
        let remote_command = format!(
            "cd {} && exec {} {}",
            posix_shell_quote(&self.working_directory),
            posix_shell_quote(&self.remote_executable),
            remote_args,
        );
        let mut command = Command::new("ssh");
        crate::hostenv::apply_to_command(&mut command);
        command
            .arg("-T")
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "StrictHostKeyChecking=yes"])
            .args(["-o", "ClearAllForwardings=yes"])
            .args(["-o", "ForwardAgent=no"])
            .args(["-o", "ForwardX11=no"])
            .args(["-o", "ForwardX11Trusted=no"])
            .args(["-o", "PermitLocalCommand=no"])
            .args(["-o", "ControlMaster=no"])
            .args(["-o", "ControlPath=none"])
            .args(["-o", "ControlPersist=no"])
            .args(["-o", "SendEnv=-*"])
            .args(["-o", "RequestTTY=no"])
            .args(["-o", "NumberOfPasswordPrompts=0"])
            .arg("-o")
            .arg(format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECONDS}"));
        if let Some(port) = self.port {
            command.arg("-p").arg(port.to_string());
        }
        command.arg(destination).arg(remote_command);
        command
    }
}

/// Whether `value` is a well-formed remote working directory: posix-absolute,
/// bounded, trimmed, and free of control bytes that could split the remote
/// shell line.
pub fn valid_remote_working_directory(value: &str) -> bool {
    bounded_trimmed(value, MAX_WORKING_DIRECTORY_BYTES)
        && value.starts_with('/')
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
}

/// Whether `path` names a working directory usable on the remote side: an
/// absolute local path, or a string that passes
/// [`valid_remote_working_directory`].
pub fn is_absolute_remote_working_directory(path: &Path) -> bool {
    path.is_absolute() || path.to_str().is_some_and(valid_remote_working_directory)
}

/// POSIX single-quote one value for the remote shell line.
pub fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn valid_host(value: &str) -> bool {
    bounded_trimmed(value, MAX_HOST_BYTES)
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'[' | b']')
        })
}

fn valid_user(value: &str) -> bool {
    bounded_trimmed(value, MAX_USER_BYTES)
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_remote_executable(value: &str) -> bool {
    bounded_trimmed(value, MAX_EXECUTABLE_BYTES)
        && !value.starts_with('-')
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
}

fn bounded_trimmed(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && value.trim() == value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest_path(segments: &[&str]) -> String {
        format!("/{}", segments.join("/"))
    }

    fn fixture() -> SshRemote {
        SshRemote::new(
            "vm.example",
            Some(2222),
            Some("agent-user"),
            &guest_path(&["opt", "hermes agent", "bin", "hermes"]),
            &guest_path(&["srv", "project's workspace"]),
        )
        .unwrap()
    }

    #[test]
    fn ssh_launch_is_batch_only_and_shell_quotes_remote_values() {
        let command = fixture().command(&["acp"]);
        assert_eq!(command.get_program(), "ssh");
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "StrictHostKeyChecking=yes"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "ForwardAgent=no"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "ControlMaster=no"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "SendEnv=-*"]));
        assert_eq!(args[args.len() - 2], "agent-user@vm.example");
        let expected_working_directory =
            posix_shell_quote(&guest_path(&["srv", "project's workspace"]));
        let expected_executable =
            posix_shell_quote(&guest_path(&["opt", "hermes agent", "bin", "hermes"]));
        assert_eq!(
            args.last().unwrap(),
            &format!("cd {expected_working_directory} && exec {expected_executable} 'acp'")
        );
    }

    #[test]
    fn remote_declarations_reject_injection_shapes() {
        assert_eq!(
            SshRemote::new(
                "-oProxyCommand=bad",
                None,
                None,
                "hermes",
                &guest_path(&["srv", "project"]),
            )
            .unwrap_err(),
            SshRemoteError::InvalidHost
        );
        assert_eq!(
            SshRemote::new(
                "vm.example",
                None,
                Some("bad user"),
                "hermes",
                &guest_path(&["srv", "project"]),
            )
            .unwrap_err(),
            SshRemoteError::InvalidUser
        );
        assert_eq!(
            SshRemote::new(
                "vm.example",
                Some(0),
                None,
                "hermes",
                &guest_path(&["srv", "project"]),
            )
            .unwrap_err(),
            SshRemoteError::InvalidPort
        );
        assert_eq!(
            SshRemote::new(
                "vm.example",
                None,
                None,
                "-rf",
                &guest_path(&["srv", "project"]),
            )
            .unwrap_err(),
            SshRemoteError::InvalidExecutable
        );
        assert_eq!(
            SshRemote::new("vm.example", None, None, "hermes", "relative/dir").unwrap_err(),
            SshRemoteError::InvalidWorkingDirectory
        );
    }

    #[test]
    fn remote_working_directories_are_posix_absolute_and_bounded() {
        assert!(valid_remote_working_directory(&guest_path(&[
            "srv", "project"
        ])));
        assert!(!valid_remote_working_directory("project"));
        assert!(!valid_remote_working_directory(
            &["C:", "workspace"].join("\\")
        ));
        let line_break = format!("{}\nother", guest_path(&["srv", "project"]));
        assert!(!valid_remote_working_directory(&line_break));
        assert!(is_absolute_remote_working_directory(Path::new(
            &guest_path(&["srv", "project"])
        )));
    }
}

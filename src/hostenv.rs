//! The host user's login-shell environment.
//!
//! A *trusted local launch* that opts into host-environment fidelity resolves
//! its values from the user's real environment — and on a GUI-launched host
//! the process environment is the launcher session's, not the user's. The
//! user's login shell is the authority: it is asked once per process for its
//! environment (same startup files as a terminal session, so PATH, proxies
//! and login state match), and every fidelity launch starts from that
//! read-only snapshot. Each launch site's own functional injections are
//! applied afterwards and therefore win over the snapshot.
//!
//! No global shell or service mutation is involved: the snapshot is read-only
//! and private to this process. Deliberately scrubbed spawns never use it.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Upper bound for one login-shell capture. A shell that cannot print its
/// environment within this window is treated as unavailable.
#[cfg(unix)]
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// Generous bound for one environment dump; a larger payload means something
/// other than `env` answered and the capture cannot be trusted.
#[cfg(unix)]
const CAPTURE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Sentinel lines bracketing the `env` dump so MOTD, prompt, and rc-file
/// noise on stdout can be filtered out.
#[cfg(unix)]
const CAPTURE_BEGIN: &str = "__A4AA_USER_SHELL_ENV_BEGIN_V1__";
#[cfg(unix)]
const CAPTURE_END: &str = "__A4AA_USER_SHELL_ENV_END_V1__";
/// Reader-poll interval while the capture is in flight.
#[cfg(unix)]
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How the process-wide snapshot was produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellEnvironmentStatus {
    /// The snapshot was captured from the user's login shell.
    Ready,
    /// Capture was unavailable; the snapshot is a copy of the host process
    /// environment, which is plain spawn behavior.
    FallbackProcessEnv,
}

struct ShellEnvironment {
    vars: BTreeMap<String, String>,
    status: ShellEnvironmentStatus,
}

static ENVIRONMENT: OnceLock<ShellEnvironment> = OnceLock::new();

fn environment() -> &'static ShellEnvironment {
    ENVIRONMENT.get_or_init(capture_or_fallback)
}

/// The captured user login-shell environment, captured lazily on first use
/// and shared by every agent launch for the rest of the process.
pub fn snapshot() -> &'static BTreeMap<String, String> {
    &environment().vars
}

/// Whether the snapshot came from the user's login shell or fell back to a
/// copy of the host process environment.
pub fn status() -> ShellEnvironmentStatus {
    environment().status
}

/// One snapshot value by name. The lookup is case-insensitive on Windows,
/// where environment variable names are.
pub fn get(key: &str) -> Option<&'static String> {
    #[cfg(windows)]
    {
        snapshot()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }
    #[cfg(not(windows))]
    {
        snapshot().get(key)
    }
}

/// Replace (unix) or overlay (Windows) a command's environment with the
/// snapshot. On unix the inherited process environment is cleared first so
/// the launched program observes exactly the user shell environment; the
/// launch site's functional injections are applied after this call and
/// therefore win. On Windows `env_clear` can strip variables that process
/// creation relies on, so the snapshot is overlaid without clearing.
///
/// The command is the standard-library `Command`; a `tokio::process::Command`
/// caller applies the snapshot through `as_std_mut()`.
pub fn apply_to_command(command: &mut Command) {
    #[cfg(not(windows))]
    command.env_clear();
    #[cfg(any(test, feature = "test-support"))]
    if let Some(vars) = SNAPSHOT_OVERRIDE.with(|cell| cell.borrow().clone()) {
        apply_snapshot_to_command(&vars, command);
        return;
    }
    apply_snapshot_to_command(snapshot(), command);
}

fn apply_snapshot_to_command(vars: &BTreeMap<String, String>, command: &mut Command) {
    for (key, value) in vars {
        command.env(key, value);
    }
}

/// Directories executable discovery searches, in order: the snapshot PATH
/// first (the user shell is the default command authority), then the host
/// process PATH as the fallback for a binary the shell snapshot does not name.
pub fn search_path_dirs() -> Vec<PathBuf> {
    let shell_path = get("PATH").map(OsString::from);
    search_path_dirs_from(shell_path.as_deref(), std::env::var_os("PATH").as_deref())
}

fn search_path_dirs_from(shell_path: Option<&OsStr>, process_path: Option<&OsStr>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut dirs = Vec::new();
    for value in [shell_path, process_path].into_iter().flatten() {
        for dir in std::env::split_paths(value) {
            if seen.insert(dir.clone()) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Whether this process is inside an OS sandbox that refuses a child
/// applying a sandbox profile of its own — the macOS seatbelt case where a
/// nested `sandbox_apply` answers EPERM. Probed once per process by
/// launching a permissive `sandbox-exec` against `/usr/bin/true`: outside a
/// sandbox the apply succeeds and the payload runs; inside a restrictive
/// profile it fails before the payload exists. A permissive inherited
/// profile still admits a nested apply and correctly reports false. Other
/// platforms nest freely and always report false.
#[cfg(target_os = "macos")]
pub fn restrictive_seatbelt() -> bool {
    static SANDBOXED: OnceLock<bool> = OnceLock::new();
    *SANDBOXED.get_or_init(|| {
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| !status.success())
            .unwrap_or(false)
    })
}

/// See the macOS variant; no nested-apply restriction exists elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn restrictive_seatbelt() -> bool {
    false
}

#[cfg(unix)]
fn capture_or_fallback() -> ShellEnvironment {
    // A capture without PATH is definitionally broken: every unix login shell
    // sets one, and an empty lookup PATH would break relative executable
    // launches that plain inheritance handles today.
    match capture_login_shell_environment() {
        Some(vars) if vars.contains_key("PATH") => ShellEnvironment {
            vars,
            status: ShellEnvironmentStatus::Ready,
        },
        _ => fallback_process_environment(),
    }
}

#[cfg(not(unix))]
fn capture_or_fallback() -> ShellEnvironment {
    // No shell capture on Windows: a GUI-launched process already carries the
    // user's environment, so the process environment itself is the snapshot.
    fallback_process_environment()
}

fn fallback_process_environment() -> ShellEnvironment {
    ShellEnvironment {
        vars: process_environment(),
        status: ShellEnvironmentStatus::FallbackProcessEnv,
    }
}

fn process_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SNAPSHOT_OVERRIDE: std::cell::RefCell<Option<BTreeMap<String, String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam: fixture executables are steered through their inherited
/// environment, which the production snapshot deliberately replaces. Pin the
/// calling thread's launch snapshot to a copy of the current process
/// environment (plus `extra` overrides) so the fixture contract is declared
/// explicitly; the previous override is restored on drop. Thread-local, so
/// parallel tests never share a pinned snapshot.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn pin_process_env_snapshot_for_testing(extra: &[(&str, &str)]) -> SnapshotOverrideGuard {
    let mut vars = process_environment();
    for (key, value) in extra {
        vars.insert((*key).to_string(), (*value).to_string());
    }
    let prior = SNAPSHOT_OVERRIDE.with(|cell| cell.replace(Some(vars)));
    SnapshotOverrideGuard(prior)
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct SnapshotOverrideGuard(Option<BTreeMap<String, String>>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for SnapshotOverrideGuard {
    fn drop(&mut self) {
        SNAPSHOT_OVERRIDE.with(|cell| {
            let _ = cell.replace(self.0.take());
        });
    }
}

/// Ask the user's login shell for its environment. The shell runs as a
/// login+interactive shell so the same startup files as a terminal session
/// define PATH, proxies, and login state. `command env` skips any user alias
/// or function shadowing `env`. BSD `env` has no `-0`, so values are split
/// per line: a value containing a newline cannot be represented and its
/// continuation lines are dropped (documented limitation).
#[cfg(unix)]
fn capture_login_shell_environment() -> Option<BTreeMap<String, String>> {
    let shell = login_shell()?;
    let mut command = Command::new(shell);
    command.args(["-l", "-i", "-c"]).arg(format!(
        "printf '\\n%s\\n' '{CAPTURE_BEGIN}'; command env; printf '%s\\n' '{CAPTURE_END}'"
    ));
    let output =
        run_bounded_capture(&mut command, CAPTURE_TIMEOUT, CAPTURE_MAX_OUTPUT_BYTES).ok()?;
    if output.timed_out || output.truncated {
        return None;
    }
    parse_environment_block(
        &String::from_utf8_lossy(&output.stdout),
        CAPTURE_BEGIN,
        CAPTURE_END,
    )
}

/// The user's shell from `$SHELL`, falling back to the platform default. A
/// configured value is trusted only when it names an existing file.
#[cfg(unix)]
fn login_shell() -> Option<PathBuf> {
    let configured = std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let candidate = configured.unwrap_or_else(|| {
        PathBuf::from(if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/bash"
        })
    });
    candidate.is_file().then_some(candidate)
}

#[cfg(unix)]
struct BoundedCapture {
    stdout: Vec<u8>,
    timed_out: bool,
    truncated: bool,
}

#[cfg(unix)]
struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Runs one trusted batch command — the login shell's `env` dump — with a
/// hard deadline, bounded captured output, and process-group cleanup. The
/// shell is spawned in its own process group so a timed-out capture cannot
/// leak descendants holding the pipes open.
#[cfg(unix)]
fn run_bounded_capture(
    command: &mut Command,
    timeout: Duration,
    max_output: usize,
) -> io::Result<BoundedCapture> {
    use command_group::CommandGroup;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.group_spawn()?;
    let stdout = child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("capture stdout unavailable"))?;
    let stderr = child
        .inner()
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("capture stderr unavailable"))?;
    let stdout_handle = thread::spawn(move || read_bounded(stdout, max_output));
    let stderr_handle = thread::spawn(move || read_bounded(stderr, max_output));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(CAPTURE_POLL_INTERVAL);
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let stdout = stdout_handle
        .join()
        .map_err(|_| io::Error::other("capture stdout reader failed"))??;
    let _ = stderr_handle.join();
    Ok(BoundedCapture {
        stdout: stdout.bytes,
        timed_out,
        truncated: stdout.truncated,
    })
}

#[cfg(unix)]
fn read_bounded(reader: impl Read, max_output: usize) -> io::Result<BoundedRead> {
    let read_limit = u64::try_from(max_output)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(max_output.min(8192));
    reader.take(read_limit).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max_output;
    bytes.truncate(max_output);
    Ok(BoundedRead { bytes, truncated })
}

/// Parse the `env` dump between the sentinel lines. Content outside the
/// sentinels (MOTD, prompt escapes, rc-file noise) is ignored; lines inside
/// without `=` (continuations of multiline values) are dropped; values split
/// on the first `=` only. Both sentinels must be present or the capture is
/// rejected.
#[cfg(unix)]
fn parse_environment_block(text: &str, begin: &str, end: &str) -> Option<BTreeMap<String, String>> {
    let mut inside = false;
    let mut closed = false;
    let mut vars = BTreeMap::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !inside {
            if line == begin {
                inside = true;
            }
            continue;
        }
        if line == end {
            closed = true;
            break;
        }
        if let Some((key, value)) = line.split_once('=') {
            if !key.is_empty() {
                vars.insert(key.to_string(), value.to_string());
            }
        }
    }
    (inside && closed).then_some(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_env_value(command: &Command, key: &str) -> Option<String> {
        command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(key))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    }

    #[cfg(unix)]
    #[test]
    fn parser_filters_noise_and_keeps_only_the_sentinel_block() {
        let text = format!(
            "motd banner\n\x1b[0m prompt escape\n{CAPTURE_BEGIN}\nPATH=/usr/bin\nHOME=/fixture/agent\n{CAPTURE_END}\ntrailing shell noise\nNOISE=ignored\n"
        );
        let vars = parse_environment_block(&text, CAPTURE_BEGIN, CAPTURE_END).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(vars.get("HOME").map(String::as_str), Some("/fixture/agent"));
        assert!(!vars.contains_key("NOISE"));
    }

    #[cfg(unix)]
    #[test]
    fn parser_keeps_empty_values_and_splits_on_the_first_equals_only() {
        let text = format!("{CAPTURE_BEGIN}\nEMPTY=\nFLAGS=--define=a=b\n{CAPTURE_END}\n");
        let vars = parse_environment_block(&text, CAPTURE_BEGIN, CAPTURE_END).unwrap();
        assert_eq!(vars.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(vars.get("FLAGS").map(String::as_str), Some("--define=a=b"));
    }

    #[cfg(unix)]
    #[test]
    fn parser_drops_lines_without_equals_inside_the_block() {
        let text =
            format!("{CAPTURE_BEGIN}\nPATH=/bin\nmultiline value continuation\n{CAPTURE_END}\n");
        let vars = parse_environment_block(&text, CAPTURE_BEGIN, CAPTURE_END).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn parser_rejects_missing_or_unclosed_sentinels() {
        assert!(parse_environment_block("PATH=/bin\n", CAPTURE_BEGIN, CAPTURE_END).is_none());
        assert!(parse_environment_block(
            &format!("{CAPTURE_BEGIN}\nPATH=/bin\n"),
            CAPTURE_BEGIN,
            CAPTURE_END,
        )
        .is_none());
        assert!(parse_environment_block(
            &format!("PATH=/bin\n{CAPTURE_END}\n"),
            CAPTURE_BEGIN,
            CAPTURE_END,
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parser_tolerates_crlf_line_endings() {
        let text = format!("{CAPTURE_BEGIN}\r\nPATH=/bin\r\n{CAPTURE_END}\r\n");
        let vars = parse_environment_block(&text, CAPTURE_BEGIN, CAPTURE_END).unwrap();
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/bin"));
    }

    #[cfg(not(windows))]
    #[test]
    fn applying_a_snapshot_replaces_the_command_environment() {
        let mut command = Command::new("fixture");
        command.env("PROCESS_ONLY_MARKER", "stale");
        let vars = BTreeMap::from([
            ("PATH".to_string(), "/snapshot/bin".to_string()),
            ("PROXY".to_string(), "http://127.0.0.1:7890".to_string()),
        ]);
        command.env_clear();
        apply_snapshot_to_command(&vars, &mut command);

        assert_eq!(command_env_value(&command, "PROCESS_ONLY_MARKER"), None);
        assert_eq!(
            command_env_value(&command, "PATH").as_deref(),
            Some("/snapshot/bin")
        );
        assert_eq!(
            command_env_value(&command, "PROXY").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[tokio::test]
    async fn a_sanitized_policy_still_wins_over_an_applied_snapshot() {
        // The deliberately scrubbed spawns apply their allowlist environment
        // after any snapshot application, so the snapshot can never widen a
        // scrubbed spawn's environment.
        let _guard =
            pin_process_env_snapshot_for_testing(&[("A4AA_TEST_SNAPSHOT_MARKER", "pinned")]);
        let mut command = Command::new("fixture");
        apply_to_command(&mut command);
        let resolved = crate::env::resolve(
            &crate::env::EnvPolicy::default(),
            &BTreeMap::new(),
            &crate::env::NoSecrets,
        )
        .await
        .unwrap();
        command.env_clear();
        for (key, value) in &resolved {
            command.env(key, value);
        }

        assert_eq!(
            command_env_value(&command, "A4AA_TEST_SNAPSHOT_MARKER"),
            None
        );
        assert_eq!(
            command_env_value(&command, "NO_COLOR").as_deref(),
            Some("1")
        );
        assert_eq!(command_env_value(&command, "TERM").as_deref(), Some("dumb"));
    }

    #[test]
    fn injections_applied_after_the_snapshot_win() {
        let mut command = Command::new("fixture");
        let vars = BTreeMap::from([("KIMI_MODEL_THINKING_EFFORT".to_string(), "low".to_string())]);
        apply_snapshot_to_command(&vars, &mut command);
        command.env("KIMI_MODEL_THINKING_EFFORT", "high");

        assert_eq!(
            command_env_value(&command, "KIMI_MODEL_THINKING_EFFORT").as_deref(),
            Some("high")
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_capture_reads_path_and_home_from_the_login_shell() {
        let Some(vars) = capture_login_shell_environment() else {
            // No usable login shell on this machine; capture failure is a
            // supported fallback path, not a test failure.
            return;
        };
        assert!(vars.contains_key("PATH"));
        assert!(vars.contains_key("HOME"));
    }

    #[test]
    fn snapshot_is_never_empty_and_matches_status() {
        assert!(!snapshot().is_empty());
        if status() == ShellEnvironmentStatus::Ready {
            #[cfg(unix)]
            assert!(snapshot().contains_key("PATH"));
        }
    }

    #[test]
    fn pinned_snapshot_override_scopes_fixture_environment_to_the_thread() {
        let mut command = Command::new("fixture");
        {
            let _pin =
                pin_process_env_snapshot_for_testing(&[("A4AA_FAKE_STEERING", "pinned-value")]);
            apply_to_command(&mut command);
            assert_eq!(
                command_env_value(&command, "A4AA_FAKE_STEERING").as_deref(),
                Some("pinned-value")
            );
        }
        let mut restored = Command::new("fixture");
        apply_to_command(&mut restored);
        assert_eq!(command_env_value(&restored, "A4AA_FAKE_STEERING"), None);
    }

    #[test]
    fn search_path_prefers_the_shell_snapshot_and_dedupes() {
        let join = |dirs: &[&str]| std::env::join_paths(dirs.iter().map(PathBuf::from)).unwrap();
        let shell_path = join(&["/shell/bin", "/shared/bin"]);
        let process_path = join(&["/process/bin", "/shared/bin"]);
        let dirs = search_path_dirs_from(Some(&shell_path), Some(&process_path));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/shell/bin"),
                PathBuf::from("/shared/bin"),
                PathBuf::from("/process/bin"),
            ]
        );
        let fallback = search_path_dirs_from(None, Some(&process_path));
        assert_eq!(
            fallback,
            vec![PathBuf::from("/process/bin"), PathBuf::from("/shared/bin")]
        );
        assert!(search_path_dirs_from(None, None).is_empty());
    }
}

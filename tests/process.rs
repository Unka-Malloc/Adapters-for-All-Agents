//! Process supervision and PTY transport (Unix): descendant cleanup,
//! interrupt grace, startup failure, byte-exact pty output, EIO-as-EOF.

#![cfg(unix)]

use a4aa::supervision::{IoPlan, StdioKind, SupervisedChild};
use a4aa::transport::pty;
use std::time::{Duration, Instant};
use tokio::process::Command;

const NULL_IO: IoPlan = IoPlan {
    stdin: StdioKind::Null,
    stdout: StdioKind::Null,
    stderr: StdioKind::Null,
};

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);
    command
}

#[tokio::test]
async fn terminate_kills_the_whole_process_tree() {
    let dir = std::env::temp_dir().join(format!("a4aa-test-tree-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pidfile = dir.join("grandchild.pid");
    let _ = std::fs::remove_file(&pidfile);

    // Spawn a grandchild inside the leader's process group and record its pid.
    let mut command = shell(&format!(
        "sleep 60 & echo $! > {}; exec sleep 60",
        pidfile.display()
    ));
    let mut child = SupervisedChild::spawn(&mut command, NULL_IO).unwrap();
    assert!(child.pid().is_some());

    let deadline = Instant::now() + Duration::from_secs(5);
    while !pidfile.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let grandchild: i32 = std::fs::read_to_string(&pidfile)
        .expect("grandchild pid file")
        .trim()
        .parse()
        .unwrap();

    child.terminate().await.unwrap();

    // The grandchild must be dead — SIGKILL is delivered before terminate
    // returns, but the orphaned zombie lingers until launchd reaps it, so
    // kill(pid, 0) needs a bounded retry before it reports ESRCH.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut result = 0;
    while Instant::now() < deadline {
        result = unsafe { libc::kill(grandchild, 0) };
        if result == -1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(result, -1, "grandchild still alive after terminate");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test]
async fn interrupt_sends_sigint_before_killing() {
    // Trap INT and exit 42 — proves the graceful path ran. The trailing
    // statement keeps `sh -c` from exec'ing `sleep`, which would discard the
    // trap and let SIGINT kill the process outright. The marker file proves
    // the trap is installed before we signal.
    let dir = std::env::temp_dir().join(format!("a4aa-test-int-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("trap-installed");
    let _ = std::fs::remove_file(&marker);

    let mut command = shell(&format!(
        "trap 'exit 42' INT; touch {}; sleep 60; exit 1",
        marker.display()
    ));
    let mut child = SupervisedChild::spawn(&mut command, NULL_IO).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(marker.exists(), "child never installed its trap");
    let status = child
        .interrupt(Duration::from_secs(3))
        .await
        .unwrap()
        .expect("interrupted child must exit");
    assert_eq!(status.code(), Some(42));
}

#[tokio::test]
async fn missing_program_fails_closed() {
    let mut command = Command::new("a4aa-no-such-binary-9f2c");
    let result = SupervisedChild::spawn(&mut command, NULL_IO);
    assert!(result.is_err());
}

#[tokio::test]
async fn pty_streams_byte_exact_output_then_eof() {
    let command = shell("printf 'one\\n'; printf 'two\\n'; exit 0");
    let (mut child, mut master) = pty::spawn(command).unwrap();
    let mut output = Vec::new();
    loop {
        let chunk = master.read().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
    }
    // Raw slave: no \r\n rewriting — output is byte-identical to a pipe.
    assert_eq!(output, b"one\ntwo\n");
    child.wait().await.unwrap();
}

#[tokio::test]
async fn pty_write_reaches_child_stdin() {
    let command = shell("cat");
    let (mut child, mut master) = pty::spawn(command).unwrap();
    master.write(b"ping\n").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let chunk = master.read().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
        if output.windows(5).any(|w| w == b"ping\n") {
            break;
        }
    }
    assert!(
        output.windows(5).any(|w| w == b"ping\n"),
        "echo missing: {output:?}"
    );
    assert!(master.resize(80, 24).is_ok());
    child.terminate().await.unwrap();
}

#[tokio::test]
async fn pty_eio_after_slave_close_is_clean_eof() {
    // Child exits immediately; the master must observe EOF, not a read error.
    let command = shell("exit 0");
    let (mut child, mut master) = pty::spawn(command).unwrap();
    child.wait().await.unwrap();
    let mut chunks = 0;
    loop {
        let chunk = master.read().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        chunks += 1;
        assert!(chunks < 16, "read loop did not terminate");
    }
    // Second EOF read stays empty instead of erroring.
    assert!(master.read().await.unwrap().is_empty());
}

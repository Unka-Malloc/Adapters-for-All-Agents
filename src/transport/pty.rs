//! Pseudo-terminal transport for CLI agents. Unix only.
//!
//! Agent CLIs that probe `isatty` degrade on a pipe: TUI rendering collapses
//! and output may only appear at exit. Attaching the child's stdin and stdout
//! to a raw-mode pty slave gives real TTY semantics while the master fd stays
//! in this process, so output streams incrementally.
//!
//! Constraints:
//! - No new platform dependencies: raw `libc` calls (`openpty`,
//!   `cfmakeraw`, `TIOCSWINSZ`).
//! - The slave runs raw (`OPOST` off), so `\n` is not rewritten and
//!   line-framed protocols parse byte-identically to pipes.
//! - stderr stays a real pipe owned by the caller.
//! - No controlling terminal (`setsid`/`TIOCSCTTY` is not needed for
//!   streaming `--print`-style lanes).

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;
use std::sync::mpsc as std_mpsc;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::error::AdapterError;
use crate::observe::{Direction, SharedObserver};
use crate::supervision::SupervisedChild;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;
use crate::bounds::READ_CHUNK;

/// Spawn `command` with stdin+stdout on a raw pty slave.
///
/// stderr is always configured as a real pipe here — terminal noise must
/// never merge into framing. `command` is consumed and dropped before
/// return: a parent-retained slave fd would keep the master from ever
/// seeing EOF.
pub fn spawn(mut command: Command) -> Result<(SupervisedChild, PtyMaster), AdapterError> {
    let (master, slave) =
        open_pty().map_err(|e| AdapterError::io("agent_pty_unavailable", "pty/open", e))?;
    make_slave_raw(slave.as_raw_fd())
        .map_err(|e| AdapterError::io("agent_pty_unavailable", "pty/termios", e))?;
    set_winsize(master.as_raw_fd(), DEFAULT_COLS, DEFAULT_ROWS)
        .map_err(|e| AdapterError::io("agent_pty_unavailable", "pty/winsize", e))?;
    command
        .stdin(Stdio::from(slave.try_clone().map_err(|e| {
            AdapterError::io("agent_pty_unavailable", "pty/dup", e)
        })?))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped());
    let child = SupervisedChild::spawn(
        &mut command,
        crate::supervision::IoPlan {
            stdin: crate::supervision::StdioKind::Preserve,
            stdout: crate::supervision::StdioKind::Preserve,
            stderr: crate::supervision::StdioKind::Preserve,
        },
    )?;
    drop(command);
    Ok((child, PtyMaster::new(master)))
}

/// Async handle on the pty master fd.
///
/// A blocking reader thread owns the fd and forwards to a tokio channel —
/// blocking reads handle the EIO→EOF race on slave close uniformly across
/// Linux and Darwin, and a chatty child can never stall the runtime.
/// Writes share the fd through `dup` on a writer thread fed by a queue.
pub struct PtyMaster {
    read_rx: mpsc::UnboundedReceiver<io::Result<Vec<u8>>>,
    write_tx: std_mpsc::SyncSender<WriteJob>,
    resize_fd: RawFd,
    observer: SharedObserver,
    eof: bool,
}

struct WriteJob {
    bytes: Vec<u8>,
    done: tokio::sync::oneshot::Sender<io::Result<()>>,
}

const WRITE_QUEUE: usize = 8;

impl PtyMaster {
    fn new(master: OwnedFd) -> Self {
        let writer_fd = master
            .try_clone()
            .expect("dup of fresh pty master cannot fail");
        let resize_fd = master.as_raw_fd();
        let (read_tx, read_rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut master = std::fs::File::from(master);
            let mut buffer = [0u8; READ_CHUNK];
            loop {
                match master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if read_tx.send(Ok(buffer[..count].to_vec())).is_err() {
                            return; // receiver gone: stop reading, drop fd
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        // Linux reports EIO once every slave fd closes;
                        // surface it as a clean EOF marker, not an error.
                        let terminal = error.raw_os_error() == Some(libc::EIO);
                        if !terminal {
                            let _ = read_tx.send(Err(error));
                        }
                        break;
                    }
                }
            }
            let _ = read_tx.send(Ok(Vec::new())); // EOF marker
        });
        let (write_tx, write_rx) = std_mpsc::sync_channel::<WriteJob>(WRITE_QUEUE);
        std::thread::spawn(move || {
            let mut writer = std::fs::File::from(writer_fd);
            while let Ok(job) = write_rx.recv() {
                let result = writer.write_all(&job.bytes).and_then(|()| writer.flush());
                let _ = job.done.send(result);
            }
        });
        Self {
            read_rx,
            write_tx,
            resize_fd,
            observer: crate::observe::none(),
            eof: false,
        }
    }

    pub fn with_observer(mut self, observer: SharedObserver) -> Self {
        self.observer = observer;
        self
    }

    /// Next chunk of terminal output; empty vec = EOF (child exit or closed).
    /// Returns `Ok(vec![])` repeatedly once closed.
    pub async fn read(&mut self) -> io::Result<Vec<u8>> {
        if self.eof {
            return Ok(Vec::new());
        }
        match self.read_rx.recv().await {
            Some(Ok(bytes)) => {
                if bytes.is_empty() {
                    self.eof = true;
                } else if let Some(observer) = self.observer.as_ref() {
                    observer.record("pty", Direction::Received, &bytes);
                }
                Ok(bytes)
            }
            Some(Err(error)) => Err(error),
            None => {
                self.eof = true;
                Ok(Vec::new())
            }
        }
    }

    /// Queue bytes to the child. Blocks (async) when the write queue is full —
    /// the bound keeps a stalled terminal from growing memory.
    pub async fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if let Some(observer) = self.observer.as_ref() {
            observer.record("pty", Direction::Sent, bytes);
        }
        let (done, wait) = tokio::sync::oneshot::channel();
        let job = WriteJob {
            bytes: bytes.to_vec(),
            done,
        };
        let send = self.write_tx.try_send(job);
        match send {
            Ok(()) => {}
            Err(std_mpsc::TrySendError::Full(job)) => {
                // Queue full: apply backpressure off the reactor.
                let tx = self.write_tx.clone();
                tokio::task::spawn_blocking(move || tx.send(job))
                    .await
                    .map_err(|_| io::Error::other("pty writer join failed"))?
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pty writer closed"))?;
            }
            Err(std_mpsc::TrySendError::Disconnected(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "pty writer closed",
                ));
            }
        }
        wait.await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pty writer dropped"))?
    }

    /// Reflow the terminal. `--print` lanes do not need this; it exists for
    /// interactive lanes.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_winsize(self.resize_fd, cols, rows)
    }
}

fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    // O_CLOEXEC is set atomically at creation on both descriptors — there
    // is no window where a concurrent fork/exec could inherit them, unlike
    // openpty() followed by a post-hoc F_SETFD.
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    unsafe {
        if libc::grantpt(master.as_raw_fd()) != 0 || libc::unlockpt(master.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
        // ptsname() writes into a shared static buffer: hold a lock from
        // the lookup until the slave fd exists so concurrent spawns cannot
        // interleave and cross-wire pty pairs.
        static PTSNAME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = PTSNAME.lock().unwrap();
        let name = libc::ptsname(master.as_raw_fd());
        if name.is_null() {
            return Err(io::Error::other("ptsname failed"));
        }
        let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, OwnedFd::from_raw_fd(slave)))
    }
}

fn make_slave_raw(fd: RawFd) -> io::Result<()> {
    unsafe {
        let mut termios = std::mem::zeroed::<libc::termios>();
        if libc::tcgetattr(fd, &mut termios) != 0 {
            return Err(io::Error::last_os_error());
        }
        // Clears OPOST (\n -> \r\n), ICANON, ECHO, ISIG on the slave.
        libc::cfmakeraw(&mut termios);
        if libc::tcsetattr(fd, libc::TCSANOW, &termios) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn set_winsize(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

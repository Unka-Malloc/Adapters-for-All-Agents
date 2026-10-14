//! Piped-stdio transport for agent processes.
//!
//! Thin wrapper over [`SupervisedChild`]: takes the child's stdin/stdout and
//! presents them as the byte halves lanes consume.

use tokio::process::{ChildStdin, ChildStdout};

use crate::error::AdapterError;
use crate::supervision::{IoPlan, StdioKind, SupervisedChild};

/// Child pipes in the arrangement protocol lanes expect: piped stdin and
/// stdout, piped stderr (drained separately, never merged into framing).
pub const PROTOCOL_IO: IoPlan = IoPlan {
    stdin: StdioKind::Piped,
    stdout: StdioKind::Piped,
    stderr: StdioKind::Piped,
};

/// Take the protocol halves out of a supervised child.
pub fn protocol_halves(
    child: &mut SupervisedChild,
) -> Result<(ChildStdin, ChildStdout, Option<tokio::process::ChildStderr>), AdapterError> {
    let stdin = child
        .stdin()
        .ok_or_else(|| AdapterError::permanent("agent_pipe_unavailable", "process/stdio"))?;
    let stdout = child
        .stdout()
        .ok_or_else(|| AdapterError::permanent("agent_pipe_unavailable", "process/stdio"))?;
    Ok((stdin, stdout, child.stderr()))
}

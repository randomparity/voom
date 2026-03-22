//! Shared subprocess utilities for executor plugins.
//!
//! Provides timeout-aware process execution used by both the MKVToolNix and
//! FFmpeg executor plugins.

use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::errors::{Result, VoomError};

/// Drain stdout and stderr pipes from a child process into buffers.
///
/// **Precondition**: The child process must have exited or been killed before
/// calling this. Calling it on a live process will deadlock if either pipe
/// fills its OS buffer.
pub fn drain_pipes(child: &mut std::process::Child) -> (Vec<u8>, Vec<u8>) {
    use std::io::Read;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout_buf).ok();
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_end(&mut stderr_buf).ok();
    }
    (stdout_buf, stderr_buf)
}

/// Run a subprocess with a timeout, killing it if it exceeds the deadline.
pub fn run_with_timeout(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
) -> Result<Output> {
    let mut child = Command::new(tool)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VoomError::ToolExecution {
            tool: tool.into(),
            message: format!("failed to spawn {tool}: {e}"),
        })?;

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let (stdout, stderr) = drain_pipes(&mut child);
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        }
        Ok(None) => {
            child.kill().ok();
            drain_pipes(&mut child);
            child.wait().ok();
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("{tool} timed out after {}s", timeout.as_secs()),
            })
        }
        Err(e) => {
            child.kill().ok();
            drain_pipes(&mut child);
            child.wait().ok();
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("error waiting for {tool}: {e}"),
            })
        }
    }
}

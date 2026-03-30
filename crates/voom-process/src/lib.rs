//! Shared subprocess utilities for executor plugins.
//!
//! Provides timeout-aware process execution used by both the `MKVToolNix` and
//! `FFmpeg` executor plugins.

use std::ffi::OsStr;
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use voom_domain::errors::{Result, VoomError};

/// Spawn reader threads for stdout and stderr pipes.
///
/// Returns join handles that yield the collected bytes. Threads are
/// spawned *before* `wait_timeout` so pipes are drained concurrently,
/// avoiding deadlock when output exceeds the OS pipe buffer.
fn spawn_pipe_readers(
    child: &mut std::process::Child,
) -> (
    std::thread::JoinHandle<Vec<u8>>,
    std::thread::JoinHandle<Vec<u8>>,
) {
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Err(e) = stdout.read_to_end(&mut buf) {
            tracing::warn!(error = %e, "failed to read child stdout");
        }
        buf
    });

    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Err(e) = stderr.read_to_end(&mut buf) {
            tracing::warn!(error = %e, "failed to read child stderr");
        }
        buf
    });

    (stdout_handle, stderr_handle)
}

/// Collect results from pipe reader threads.
fn join_pipe_readers(
    stdout_handle: std::thread::JoinHandle<Vec<u8>>,
    stderr_handle: std::thread::JoinHandle<Vec<u8>>,
) -> (Vec<u8>, Vec<u8>) {
    let stdout = stdout_handle.join().unwrap_or_else(|e| {
        tracing::warn!("stdout pipe reader panicked: {e:?}");
        Vec::new()
    });
    let stderr = stderr_handle.join().unwrap_or_else(|e| {
        tracing::warn!("stderr pipe reader panicked: {e:?}");
        Vec::new()
    });
    (stdout, stderr)
}

/// Run a subprocess with a timeout, killing it if it exceeds the deadline.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if the process fails or times out.
pub fn run_with_timeout(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
) -> Result<Output> {
    run_with_timeout_env(tool, args, timeout, &[])
}

/// Run a subprocess with a timeout and extra environment variables.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if the process fails or times out.
pub fn run_with_timeout_env(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    env_vars: &[(&str, &str)],
) -> Result<Output> {
    let mut cmd = Command::new(tool);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (key, val) in env_vars {
        cmd.env(key, val);
    }
    let mut child = cmd.spawn().map_err(|e| VoomError::ToolExecution {
        tool: tool.into(),
        message: format!("failed to spawn {tool}: {e}"),
    })?;

    // Spawn reader threads before waiting so pipes drain concurrently.
    let (stdout_handle, stderr_handle) = spawn_pipe_readers(&mut child);

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let (stdout, stderr) = join_pipe_readers(stdout_handle, stderr_handle);
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        }
        Ok(None) => {
            if let Err(e) = child.kill() {
                tracing::warn!(tool = tool, error = %e, "failed to kill child process");
            }
            child.wait().ok();
            join_pipe_readers(stdout_handle, stderr_handle);
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("{tool} timed out after {}s", timeout.as_secs()),
            })
        }
        Err(e) => {
            if let Err(kill_err) = child.kill() {
                tracing::warn!(
                    tool = tool,
                    error = %kill_err,
                    "failed to kill child process"
                );
            }
            child.wait().ok();
            join_pipe_readers(stdout_handle, stderr_handle);
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("error waiting for {tool}: {e}"),
            })
        }
    }
}

/// Read all bytes from an optional tokio `ChildStdout`/`ChildStderr`.
async fn read_child_pipe<R>(pipe: Option<R>) -> Vec<u8>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    if let Err(e) = pipe.read_to_end(&mut buf).await {
        tracing::warn!(error = %e, "failed to read child pipe");
    }
    buf
}

/// Async cancellable subprocess execution via `tokio::process`.
///
/// Selects on timeout, cancellation token, and child exit. If the
/// token fires the child is killed immediately.
///
/// # Errors
/// Returns `VoomError::ToolExecution` on timeout, spawn failure,
/// or cancellation.
pub async fn run_cancellable(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    token: &tokio_util::sync::CancellationToken,
) -> Result<Output> {
    run_cancellable_env(tool, args, timeout, &[], token).await
}

/// Async cancellable subprocess with extra environment variables.
///
/// See [`run_cancellable`] for details.
pub async fn run_cancellable_env(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    env_vars: &[(&str, &str)],
    token: &tokio_util::sync::CancellationToken,
) -> Result<Output> {
    use tokio::process::Command as TokioCommand;

    let mut cmd = TokioCommand::new(tool);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, val) in env_vars {
        cmd.env(key, val);
    }
    let mut child = cmd.spawn().map_err(|e| VoomError::ToolExecution {
        tool: tool.into(),
        message: format!("failed to spawn {tool}: {e}"),
    })?;

    // Use child.wait() (borrows) instead of wait_with_output() (moves)
    // so we can still call child.kill() in cancellation branches.
    tokio::select! {
        status = child.wait() => {
            let status = status.map_err(|e| VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("error waiting for {tool}: {e}"),
            })?;
            let stdout = read_child_pipe(child.stdout.take()).await;
            let stderr = read_child_pipe(child.stderr.take()).await;
            Ok(Output { status, stdout, stderr })
        }
        () = tokio::time::sleep(timeout) => {
            let _ = child.kill().await;
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!(
                    "{tool} timed out after {}s",
                    timeout.as_secs()
                ),
            })
        }
        () = token.cancelled() => {
            let _ = child.kill().await;
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("{tool} cancelled"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_echo_succeeds() {
        let output = run_with_timeout("echo", &["hello"], Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn run_nonexistent_tool_returns_error() {
        let err =
            run_with_timeout("nonexistent_tool_xyz", &["-v"], Duration::from_secs(5)).unwrap_err();
        match &err {
            VoomError::ToolExecution { tool, message } => {
                assert_eq!(tool, "nonexistent_tool_xyz");
                assert!(message.contains("failed to spawn"));
            }
            other => panic!("expected ToolExecution, got: {other}"),
        }
    }

    #[test]
    fn test_run_with_timeout_env() {
        let output = run_with_timeout_env(
            "env",
            &[] as &[&str],
            Duration::from_secs(5),
            &[("VOOM_TEST_VAR", "hello_gpu")],
        )
        .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("VOOM_TEST_VAR=hello_gpu"),
            "env output should contain the set var, got: {stdout}"
        );
    }

    #[test]
    fn run_with_timeout_kills_slow_process() {
        // sleep 60 with a 1-second timeout should be killed
        let err = run_with_timeout("sleep", &["60"], Duration::from_secs(1)).unwrap_err();
        match &err {
            VoomError::ToolExecution { tool, message } => {
                assert_eq!(tool, "sleep");
                assert!(message.contains("timed out"));
            }
            other => panic!("expected ToolExecution timeout, got: {other}"),
        }
    }

    #[tokio::test]
    async fn cancellable_echo_succeeds() {
        let token = tokio_util::sync::CancellationToken::new();
        let output = run_cancellable_env("echo", &["hello"], Duration::from_secs(5), &[], &token)
            .await
            .expect("echo should succeed");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[tokio::test]
    async fn cancellable_kills_on_cancel() {
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let err = run_cancellable_env("sleep", &["60"], Duration::from_secs(30), &[], &token)
            .await
            .unwrap_err();
        match &err {
            VoomError::ToolExecution { tool, message } => {
                assert_eq!(tool, "sleep");
                assert!(
                    message.contains("cancelled"),
                    "expected 'cancelled' in message, got: {message}"
                );
            }
            other => panic!("expected ToolExecution, got: {other}"),
        }
    }

    #[tokio::test]
    async fn cancellable_times_out() {
        let token = tokio_util::sync::CancellationToken::new();
        let err = run_cancellable_env("sleep", &["60"], Duration::from_secs(1), &[], &token)
            .await
            .unwrap_err();
        match &err {
            VoomError::ToolExecution { tool, message } => {
                assert_eq!(tool, "sleep");
                assert!(
                    message.contains("timed out"),
                    "expected 'timed out' in message, got: {message}"
                );
            }
            other => panic!("expected ToolExecution timeout, got: {other}"),
        }
    }
}

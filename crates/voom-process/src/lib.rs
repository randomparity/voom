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
                message: format!("{tool} timed out after {:.1}s", timeout.as_secs_f64()),
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

/// Format a tool invocation as a shell-reproducible command string.
///
/// Args containing spaces, quotes, or shell metacharacters are
/// single-quoted. Simple args pass through unquoted.
pub fn shell_quote_args(tool: &str, args: &[impl AsRef<str>]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(tool.to_string());
    for arg in args {
        let s = arg.as_ref();
        if s.is_empty()
            || s.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}[]<>?*~".contains(c))
        {
            let escaped = s.replace('\'', "'\\''");
            parts.push(format!("'{escaped}'"));
        } else {
            parts.push(s.to_string());
        }
    }
    parts.join(" ")
}

/// Extract the last N non-empty lines from a byte buffer (stderr output).
pub fn stderr_tail(bytes: &[u8], max_lines: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
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

    // Take pipes before waiting so they drain concurrently with the
    // child, avoiding deadlock when output exceeds the OS pipe buffer.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    tokio::select! {
        result = async {
            let (stdout, stderr, status) = tokio::join!(
                read_child_pipe(stdout_pipe),
                read_child_pipe(stderr_pipe),
                child.wait(),
            );
            (stdout, stderr, status)
        } => {
            let (stdout, stderr, status) = result;
            let status = status.map_err(|e| VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("error waiting for {tool}: {e}"),
            })?;
            Ok(Output { status, stdout, stderr })
        }
        () = tokio::time::sleep(timeout) => {
            let _ = child.kill().await;
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!(
                    "{tool} timed out after {:.1}s",
                    timeout.as_secs_f64()
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

    #[tokio::test]
    async fn cancellable_drains_large_output() {
        // Generate >128KB of output (exceeds typical OS pipe buffer of
        // 64KB). Before the fix, this would deadlock because the child
        // blocks writing to full pipes while we wait for exit.
        let token = tokio_util::sync::CancellationToken::new();
        let output = run_cancellable_env(
            "dd",
            &["if=/dev/zero", "bs=1024", "count=256"],
            Duration::from_secs(10),
            &[],
            &token,
        )
        .await
        .expect("dd should succeed");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 256 * 1024, "expected 256KB of output");
    }
}

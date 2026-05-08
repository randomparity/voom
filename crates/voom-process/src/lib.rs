//! Shared subprocess utilities for executor plugins.
//!
//! Provides timeout-aware process execution used by both the `MKVToolNix` and
//! `FFmpeg` executor plugins.

use std::ffi::OsStr;
use std::io::{ErrorKind, Read};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use voom_domain::errors::{Result, VoomError};

/// Default maximum bytes retained from each captured process stream.
pub const DEFAULT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

/// Per-stream subprocess output capture limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureConfig {
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

/// Options for cancellable subprocess execution.
#[derive(Clone, Copy, Debug)]
pub struct CancellableOptions<'a> {
    pub env_vars: &'a [(&'a str, &'a str)],
    pub token: &'a tokio_util::sync::CancellationToken,
    pub capture: CaptureConfig,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_stdout_bytes: DEFAULT_CAPTURE_LIMIT_BYTES,
            max_stderr_bytes: DEFAULT_CAPTURE_LIMIT_BYTES,
        }
    }
}

struct BoundedCapture {
    output: Vec<u8>,
    truncated_bytes: usize,
    max_bytes: usize,
}

impl BoundedCapture {
    fn new(max_bytes: usize) -> Self {
        Self {
            output: Vec::with_capacity(max_bytes.min(8192)),
            truncated_bytes: 0,
            max_bytes,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let remaining = self.max_bytes.saturating_sub(self.output.len());
        let retained = remaining.min(chunk.len());
        self.output.extend_from_slice(&chunk[..retained]);
        self.truncated_bytes = self.truncated_bytes.saturating_add(chunk.len() - retained);
    }

    fn finish(mut self) -> Vec<u8> {
        append_truncation_marker(&mut self.output, self.truncated_bytes);
        self.output
    }
}

fn append_truncation_marker(buf: &mut Vec<u8>, truncated_bytes: usize) {
    if truncated_bytes == 0 {
        return;
    }
    if !buf.is_empty() && !buf.ends_with(b"\n") {
        buf.push(b'\n');
    }
    buf.extend_from_slice(format!("...[truncated {truncated_bytes} bytes]").as_bytes());
}

/// Read a stream to EOF while retaining at most `max_bytes` of original data.
///
/// The returned buffer includes a truncation marker when bytes were discarded.
/// Prefer `run_with_timeout*` for subprocess execution; this helper exists for
/// integrations that already own process lifecycle management.
///
/// # Errors
/// Returns any I/O error produced while reading from `reader`.
pub fn read_bounded<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Vec<u8>>
where
    R: Read,
{
    let mut capture = BoundedCapture::new(max_bytes);
    let mut chunk = [0u8; 8192];

    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        capture.push(&chunk[..read]);
    }

    Ok(capture.finish())
}

fn spawn_error(tool: &str, error: std::io::Error) -> VoomError {
    if error.kind() == ErrorKind::NotFound {
        VoomError::ToolNotFound {
            tool: tool.to_string(),
        }
    } else {
        VoomError::ToolExecution {
            tool: tool.into(),
            message: format!("failed to spawn {tool}: {error}"),
        }
    }
}

/// Spawn reader threads for stdout and stderr pipes.
///
/// Returns join handles that yield the collected bytes. Threads are
/// spawned *before* `wait_timeout` so pipes are drained concurrently,
/// avoiding deadlock when output exceeds the OS pipe buffer.
fn spawn_pipe_readers(
    child: &mut std::process::Child,
    capture: CaptureConfig,
) -> (
    std::thread::JoinHandle<Vec<u8>>,
    std::thread::JoinHandle<Vec<u8>>,
) {
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    let stdout_handle = std::thread::spawn(move || {
        read_bounded(&mut stdout, capture.max_stdout_bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read child stdout");
            Vec::new()
        })
    });

    let stderr_handle = std::thread::spawn(move || {
        read_bounded(&mut stderr, capture.max_stderr_bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read child stderr");
            Vec::new()
        })
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
    run_with_timeout_env_config(tool, args, timeout, &[], CaptureConfig::default())
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
    run_with_timeout_env_config(tool, args, timeout, env_vars, CaptureConfig::default())
}

/// Run a subprocess with a timeout and configured output capture limits.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if the process fails or times out.
pub fn run_with_timeout_config(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    capture: CaptureConfig,
) -> Result<Output> {
    run_with_timeout_env_config(tool, args, timeout, &[], capture)
}

/// Run a subprocess with a timeout, extra environment variables, and capture limits.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if the process fails or times out.
pub fn run_with_timeout_env_config(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    env_vars: &[(&str, &str)],
    capture: CaptureConfig,
) -> Result<Output> {
    let mut cmd = Command::new(tool);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (key, val) in env_vars {
        cmd.env(key, val);
    }
    let mut child = cmd.spawn().map_err(|e| spawn_error(tool, e))?;

    // Spawn reader threads before waiting so pipes drain concurrently.
    let (stdout_handle, stderr_handle) = spawn_pipe_readers(&mut child, capture);

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

/// Run a subprocess and return whether it exits successfully before the timeout.
#[must_use]
pub fn probe_tool_status(tool: &str, args: &[impl AsRef<OsStr>], timeout: Duration) -> bool {
    run_with_timeout(tool, args, timeout).is_ok_and(|o| o.status.success())
}

/// Run a subprocess with extra environment variables and return whether it succeeds.
#[must_use]
pub fn probe_tool_status_env(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    env_vars: &[(&str, &str)],
) -> bool {
    run_with_timeout_env(tool, args, timeout, env_vars).is_ok_and(|o| o.status.success())
}

/// Format a tool invocation as a shell-reproducible command string.
///
/// Args containing spaces, quotes, or shell metacharacters are
/// single-quoted. Simple args pass through unquoted.
pub fn shell_quote_args(tool: &str, args: &[impl AsRef<str>]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    // Quote the tool name if it contains shell metacharacters (e.g. spaces in path)
    if tool.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}[]<>?*~".contains(c)) {
        let escaped = tool.replace('\'', "'\\''");
        parts.push(format!("'{escaped}'"));
    } else {
        parts.push(tool.to_string());
    }
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
#[must_use]
pub fn stderr_tail(bytes: &[u8], max_lines: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Read all bytes from an optional tokio `ChildStdout`/`ChildStderr`.
async fn read_child_pipe<R>(pipe: Option<R>, max_bytes: usize) -> Vec<u8>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };

    let mut capture = BoundedCapture::new(max_bytes);
    let mut chunk = [0u8; 8192];

    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(read) => read,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read child pipe");
                break;
            }
        };
        if read == 0 {
            break;
        }
        capture.push(&chunk[..read]);
    }

    capture.finish()
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
    run_cancellable_env_config(
        tool,
        args,
        timeout,
        CancellableOptions {
            env_vars: &[],
            token,
            capture: CaptureConfig::default(),
        },
    )
    .await
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
    run_cancellable_env_config(
        tool,
        args,
        timeout,
        CancellableOptions {
            env_vars,
            token,
            capture: CaptureConfig::default(),
        },
    )
    .await
}

/// Async cancellable subprocess with configured output capture limits.
///
/// See [`run_cancellable`] for details.
pub async fn run_cancellable_config(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    token: &tokio_util::sync::CancellationToken,
    capture: CaptureConfig,
) -> Result<Output> {
    run_cancellable_env_config(
        tool,
        args,
        timeout,
        CancellableOptions {
            env_vars: &[],
            token,
            capture,
        },
    )
    .await
}

/// Async cancellable subprocess with extra environment variables and capture limits.
///
/// See [`run_cancellable`] for details.
pub async fn run_cancellable_env_config(
    tool: &str,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    options: CancellableOptions<'_>,
) -> Result<Output> {
    use tokio::process::Command as TokioCommand;

    let mut cmd = TokioCommand::new(tool);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, val) in options.env_vars {
        cmd.env(key, val);
    }
    let mut child = cmd.spawn().map_err(|e| spawn_error(tool, e))?;

    // Take pipes before waiting so they drain concurrently with the
    // child, avoiding deadlock when output exceeds the OS pipe buffer.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    tokio::select! {
        result = async {
            let (stdout, stderr, status) = tokio::join!(
                read_child_pipe(stdout_pipe, options.capture.max_stdout_bytes),
                read_child_pipe(stderr_pipe, options.capture.max_stderr_bytes),
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
        () = options.token.cancelled() => {
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
            VoomError::ToolNotFound { tool } => {
                assert_eq!(tool, "nonexistent_tool_xyz");
            }
            other => panic!("expected ToolNotFound, got: {other}"),
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
    fn run_with_timeout_truncates_stdout_at_configured_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("stdout-flood.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\npython3 - <<'PY'\nimport sys\nsys.stdout.write('a' * 40)\nPY\n",
        )
        .expect("write script");
        make_executable(&script);

        let output = run_with_timeout_env_config(
            script.to_str().expect("utf8 path"),
            &[] as &[&str],
            Duration::from_secs(5),
            &[],
            CaptureConfig {
                max_stdout_bytes: 10,
                max_stderr_bytes: DEFAULT_CAPTURE_LIMIT_BYTES,
            },
        )
        .expect("script succeeds");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "aaaaaaaaaa\n...[truncated 30 bytes]"
        );
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn run_with_timeout_truncates_stderr_at_configured_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("stderr-flood.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\npython3 - <<'PY'\nimport sys\nsys.stderr.write('first\\nsecond\\nthird\\nfourth\\n')\nPY\n",
        )
        .expect("write script");
        make_executable(&script);

        let output = run_with_timeout_env_config(
            script.to_str().expect("utf8 path"),
            &[] as &[&str],
            Duration::from_secs(5),
            &[],
            CaptureConfig {
                max_stdout_bytes: DEFAULT_CAPTURE_LIMIT_BYTES,
                max_stderr_bytes: 13,
            },
        )
        .expect("script succeeds");

        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "first\nsecond\n...[truncated 13 bytes]"
        );
    }

    #[test]
    fn stderr_tail_keeps_truncation_marker_visible() {
        let mut stderr = b"line1\nline2\nline3".to_vec();
        append_truncation_marker(&mut stderr, 25);

        let tail = stderr_tail(&stderr, 2);

        assert_eq!(tail, "line3\n...[truncated 25 bytes]");
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
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
    async fn run_cancellable_truncates_stdout_at_configured_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("async-stdout-flood.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\npython3 - <<'PY'\nimport sys\nsys.stdout.write('b' * 35)\nPY\n",
        )
        .expect("write script");
        make_executable(&script);

        let token = tokio_util::sync::CancellationToken::new();
        let output = run_cancellable_env_config(
            script.to_str().expect("utf8 path"),
            &[] as &[&str],
            Duration::from_secs(5),
            CancellableOptions {
                env_vars: &[],
                token: &token,
                capture: CaptureConfig {
                    max_stdout_bytes: 8,
                    max_stderr_bytes: DEFAULT_CAPTURE_LIMIT_BYTES,
                },
            },
        )
        .await
        .expect("script succeeds");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "bbbbbbbb\n...[truncated 27 bytes]"
        );
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

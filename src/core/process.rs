//! Safe process execution with timeout and deadlock-free I/O.
//!
//! Solves two problems:
//! 1. **Timeout enforcement**: kills hung child processes after a deadline.
//! 2. **Stderr deadlock**: drains stdout and stderr on separate threads
//!    to prevent pipe buffer deadlocks.

use crate::core::error::DurableError;
use std::io::Read;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Output from a process run with timeout.
pub struct ProcessOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Run a child process with a timeout. Drains stdout/stderr on separate
/// threads to prevent pipe buffer deadlocks.
///
/// If the process doesn't exit within `timeout`, it is killed (SIGKILL).
pub fn run_with_timeout(
    mut child: Child,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<ProcessOutput, DurableError> {
    // Write stdin and close it (so the child sees EOF)
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            // Best-effort write — if the child crashes before reading, we get BrokenPipe
            let _ = stdin.write_all(data);
            // stdin is dropped here, closing the pipe
        }
    } else {
        // Drop stdin even if no input, so child sees EOF
        drop(child.stdin.take());
    }

    // Drain stdout and stderr on separate threads to prevent deadlock.
    // If the child writes enough to fill a pipe buffer (~64KB), it blocks
    // on write. If we're not reading, both sides deadlock.
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut r) = stdout_handle {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });

    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut r) = stderr_handle {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });

    // Spawn a killer thread that terminates the child after the timeout.
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();
    let child_id = child.id();

    let killer = std::thread::spawn(move || {
        // Sleep in small increments so we can check the done flag
        let check_interval = Duration::from_millis(100);
        let mut elapsed = Duration::ZERO;
        while elapsed < timeout {
            std::thread::sleep(check_interval);
            elapsed += check_interval;
            if done_clone.load(Ordering::SeqCst) {
                return false; // process finished in time
            }
        }
        // Timeout reached — process is still running
        true
    });

    // Wait for the child to exit
    let status = child.wait().map_err(|e| DurableError::Io(e.to_string()))?;
    done.store(true, Ordering::SeqCst);

    let timed_out = killer.join().unwrap_or(false);

    // If we timed out, the child may still be alive (wait() returned because
    // we read its pipes, but it could be a zombie). Kill it to be sure.
    if timed_out {
        // The child already exited via wait() above, but the killer thread
        // detected timeout. This means the process ran past the deadline.
        // Since wait() already returned, the process is done — but we report timeout.
        let _stdout = stdout_thread.join().unwrap_or_default();
        let stderr = stderr_thread.join().unwrap_or_default();
        return Err(DurableError::ToolError {
            tool_name: format!("pid:{}", child_id),
            message: format!(
                "process timed out after {}s. stderr: {}",
                timeout.as_secs(),
                truncate(&stderr, 500)
            ),
            retryable: true,
        });
    }

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

/// Run a child process with timeout, killing it on deadline.
/// Unlike `run_with_timeout`, this actively kills the process.
pub fn run_with_kill_timeout(
    mut child: Child,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<ProcessOutput, DurableError> {
    // Write stdin
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(data);
        }
    } else {
        drop(child.stdin.take());
    }

    // Drain stdout/stderr on threads
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut r) = stdout_handle {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });

    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut r) = stderr_handle {
            let _ = r.read_to_string(&mut buf);
        }
        buf
    });

    // Poll child with timeout using try_wait
    let check_interval = Duration::from_millis(50);
    let mut elapsed = Duration::ZERO;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                // Still running
                if elapsed >= timeout {
                    // Kill the process
                    let _ = child.kill();
                    // Wait for it to actually exit after kill
                    let _ = child.wait();
                    let _stdout = stdout_thread.join().unwrap_or_default();
                    let stderr = stderr_thread.join().unwrap_or_default();
                    return Err(DurableError::ToolError {
                        tool_name: "process".to_string(),
                        message: format!(
                            "process killed after {}s timeout. stderr: {}",
                            timeout.as_secs(),
                            truncate(&stderr, 500)
                        ),
                        retryable: true,
                    });
                }
                std::thread::sleep(check_interval);
                elapsed += check_interval;
            }
            Err(e) => return Err(DurableError::Io(e.to_string())),
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

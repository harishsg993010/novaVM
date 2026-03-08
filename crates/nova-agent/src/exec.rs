//! Process execution inside the guest.
//!
//! Provides functions to spawn child processes with captured output,
//! environment variable control, and working directory selection.

use std::collections::HashMap;
use std::process::Command;

use crate::error::{AgentError, Result};
use crate::protocol::Response;

/// Result of executing a command.
#[derive(Debug)]
pub struct ExecOutput {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Execute a command inside the guest and capture its output.
///
/// The command is run as a child process with stdout and stderr captured.
/// If the process fails to start, an error is returned. If it starts but
/// exits with a non-zero code, the exit code is returned in `ExecOutput`.
pub fn execute_command(
    args: &[String],
    env: &HashMap<String, String>,
    workdir: Option<&str>,
) -> Result<ExecOutput> {
    if args.is_empty() {
        return Err(AgentError::Exec("empty command".to_string()));
    }

    let program = &args[0];
    let cmd_args = &args[1..];

    tracing::info!(program, ?cmd_args, "executing command");

    let mut cmd = Command::new(program);
    cmd.args(cmd_args);

    // Apply environment variables.
    for (key, value) in env {
        cmd.env(key, value);
    }

    // Apply working directory.
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }

    let output = cmd
        .output()
        .map_err(|e| AgentError::Exec(format!("failed to execute '{}': {}", program, e)))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    tracing::info!(program, exit_code, "command completed");

    Ok(ExecOutput {
        exit_code,
        stdout,
        stderr,
    })
}

/// Execute a command and return it as a protocol [`Response`].
pub fn execute_to_response(
    args: &[String],
    env: &HashMap<String, String>,
    workdir: Option<&str>,
) -> Response {
    match execute_command(args, env, workdir) {
        Ok(output) => Response::ExecResult {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        },
        Err(e) => Response::Error {
            message: e.to_string(),
        },
    }
}

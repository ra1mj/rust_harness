//! The shell capability seam: definition, local provider, and plugin.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use rh_core::{Context, Disposers, Plugin};
use rh_tool::ToolError;

/// The result of running a shell command.
#[derive(Debug, Clone, Serialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Service Definition: run a shell command.
#[async_trait]
pub trait Shell: Send + Sync {
    async fn run(&self, command: &str, cwd: &Path) -> Result<CommandOutput, ToolError>;
}

/// Service Provider: the local shell.
pub struct LocalShell;

/// Resolve a shell program and its `-c <command>`-style arguments.
///
/// Tries the common Unix shells in order (so a sandbox with only `/bin/sh`
/// still works), and falls back to `cmd /C` on Windows. This fixes the
/// "No such file or directory (os error 2)" failure when `sh` is not on
/// `PATH`.
fn shell_invocation(command: &str) -> (&'static str, Vec<String>) {
    #[cfg(not(windows))]
    {
        for program in ["sh", "bash", "/bin/sh", "/bin/bash", "/usr/bin/env"] {
            if program.contains('/') || which_exists(program) {
                let mut args = vec!["-c".to_string(), command.to_string()];
                if program == "/usr/bin/env" {
                    // `env sh -c <command>`
                    args = vec!["sh".to_string(), "-c".to_string(), command.to_string()];
                }
                return (program, args);
            }
        }
        // Last resort: rely on the platform default; the error will name it.
        ("sh", vec!["-c".to_string(), command.to_string()])
    }
    #[cfg(windows)]
    {
        ("cmd", vec!["/C".to_string(), command.to_string()])
    }
}

#[cfg(not(windows))]
fn which_exists(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .output()
        .map(|_| true)
        .unwrap_or(false)
        || std::process::Command::new("which")
            .arg(program)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

#[async_trait]
impl Shell for LocalShell {
    async fn run(&self, command: &str, cwd: &Path) -> Result<CommandOutput, ToolError> {
        let (program, args) = shell_invocation(command);
        let output = tokio::process::Command::new(program)
            .args(&args)
            .current_dir(cwd)
            .output()
            .await
            .map_err(|e| {
                ToolError::execution(format!(
                    "failed to spawn `{program}` (is a shell installed?): {e}"
                ))
            })?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
        })
    }
}

/// Mounts the local shell provider.
pub struct ShellPlugin;

impl Plugin for ShellPlugin {
    fn name(&self) -> &'static str {
        "shell:local"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let shell: Arc<dyn Shell> = Arc::new(LocalShell);
        Ok(vec![ctx.provide_named("Shell(local)", shell)])
    }
}

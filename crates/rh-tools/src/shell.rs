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

/// Service Provider: the local shell (`sh -c`).
pub struct LocalShell;

#[async_trait]
impl Shell for LocalShell {
    async fn run(&self, command: &str, cwd: &Path) -> Result<CommandOutput, ToolError> {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .output()
            .await
            .map_err(ToolError::from)?;
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

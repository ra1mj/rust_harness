//! rh-tools — built-in tools and their capability seams.
//!
//! Demonstrates the DeepSeek Harness capability seam inside Grok Build's
//! tool shape. A **seam** has three roles:
//!
//! * **Service Definition** — a trait ([`Shell`], [`FileSystem`]).
//! * **Service Provider** — an implementation registered on the context
//!   ([`LocalShell`], [`LocalFileSystem`]).
//! * **Consumer** — a tool ([`BashTool`], [`FsReadTool`], [`FsWriteTool`])
//!   that resolves the service from its [`ToolCallContext`].
//!
//! Swapping the provider (e.g. pointing `Shell` at a remote sandbox)
//! changes the tool's behavior without touching the tool — the same reason
//! one provider swap in `dsh` moves bash, PTY, and LSP together.

mod fs;
mod shell;
mod tools;

pub use fs::{FileSystem, FileSystemPlugin, LocalFileSystem};
pub use shell::{CommandOutput, LocalShell, Shell, ShellPlugin};
pub use tools::{BashTool, FsReadTool, FsWriteTool, TodoWriteTool, ToolsPlugin};

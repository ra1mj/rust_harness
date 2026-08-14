//! rh-tool — the unified tool layer.
//!
//! This crate ports Grok Build's single, streaming `Tool` trait into the
//! plugin seam from `rh-core`. Every tool — shell, file I/O, todo, and
//! later MCP adapters or subagents — implements one [`Tool`] trait and
//! registers on a shared [`ToolRegistry`]. A tool is the *Consumer* role of
//! a capability seam: it resolves a capability service (e.g. a `Shell`)
//! from the [`Context`](rh_core::Context) it is handed, so swapping the
//! service provider changes the tool without changing the tool.
//!
//! The stream contract mirrors Grok Build: a tool call produces zero or
//! more [`ToolStreamItem::Progress`] items followed by exactly one
//! [`ToolStreamItem::Terminal`].

mod bridge;
mod call;
mod description;
mod error;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde_json::Value;

pub use bridge::ToolRegistry;
pub use call::ToolCallContext;
pub use description::{ToolCapabilities, ToolConcurrency, ToolDescription};
pub use error::{ToolError, ToolErrorKind};

/// Stable identity used to route a model's tool call to a tool.
pub type ToolId = String;

/// Identity of a single tool invocation within a session.
pub type ToolCallId = String;

/// One item in a tool call stream: zero or more `Progress` items, then
/// exactly one `Terminal`.
#[derive(Debug, Clone)]
pub enum ToolStreamItem {
    /// Intermediate progress (a log line, partial output).
    Progress { text: String },
    /// Terminal result; always the last item.
    Terminal(Result<Value, ToolError>),
}

/// An opaque, pinned stream of [`ToolStreamItem`].
pub type ToolStream = Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>;

/// The unified tool trait used by every tool source.
///
/// Implement either [`Tool::run`] (blocking) or [`Tool::execute`]
/// (streaming). The runtime only ever invokes [`Tool::execute`]; the
/// default implementation wraps `run` into a single-item terminal stream.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Stable identity used to route to this tool.
    fn id(&self) -> ToolId;

    /// Model-facing name, description, and argument JSON Schema.
    fn description(&self) -> ToolDescription;

    /// Per-tool capability flags.
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::default()
    }

    /// Per-turn listing predicate: return `false` to exclude this tool from
    /// the model-facing manifest for a given turn.
    fn should_list(&self, _ctx: &ToolCallContext) -> bool {
        true
    }

    /// Streaming entry point. The default wraps [`Tool::run`] into a
    /// single-item terminal stream, so blocking tools only override `run`.
    async fn execute(&self, ctx: &ToolCallContext, args: Value) -> ToolStream {
        let result = self.run(ctx, args).await;
        terminal_only(result)
    }

    /// Blocking convenience entry point. Default fails loudly, so a tool
    /// that overrides neither method surfaces as `not_implemented` on the
    /// first call rather than silently doing nothing.
    async fn run(&self, _ctx: &ToolCallContext, _args: Value) -> Result<Value, ToolError> {
        Err(ToolError::not_implemented(self.id()))
    }
}

/// Build a tool stream whose only item is a terminal result.
pub fn terminal_only(result: Result<Value, ToolError>) -> ToolStream {
    Box::pin(futures::stream::once(async move {
        ToolStreamItem::Terminal(result)
    }))
}

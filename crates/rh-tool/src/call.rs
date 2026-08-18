//! The per-call context handed to a tool.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use rh_core::Context;

use crate::ToolCallId;

/// Everything a tool may need during a single call.
///
/// Crucially it carries the shared [`Context`], so a tool (the *Consumer*
/// role) can resolve the capability services it depends on — a `Shell`, a
/// `FileSystem`, a `TodoList` — without importing the loop. This is what
/// makes a provider swap change the tool's behavior in place.
#[derive(Clone)]
pub struct ToolCallContext {
    /// The shared composition context (services + events).
    pub context: Context,
    /// Id of the session this call belongs to.
    pub session_id: String,
    /// Working directory for path-relative operations.
    pub cwd: PathBuf,
    /// Id of this specific tool invocation.
    pub tool_call_id: ToolCallId,
    /// Open-ended, type-keyed extensions (e.g. cancellation tokens).
    pub extensions: HashMap<String, Value>,
}

impl ToolCallContext {
    /// Build a call context for a fresh call.
    pub fn new(context: Context, session_id: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            context,
            session_id: session_id.into(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            tool_call_id: tool_call_id.into(),
            extensions: HashMap::new(),
        }
    }

    /// Override the working directory (the session's workspace root).
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = cwd;
        self
    }

    /// Resolve a capability service, or return a `missing_service` error.
    pub fn service<T: rh_core::Service + ?Sized>(
        &self,
        what: &str,
    ) -> Result<std::sync::Arc<T>, crate::ToolError> {
        self.context
            .service::<T>()
            .ok_or_else(|| crate::ToolError::missing_service(what))
    }
}

impl Default for ToolCallContext {
    fn default() -> Self {
        Self {
            context: Context::new(),
            session_id: String::new(),
            cwd: PathBuf::from("."),
            tool_call_id: String::new(),
            extensions: HashMap::new(),
        }
    }
}

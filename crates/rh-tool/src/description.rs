//! Tool metadata: description, argument schema, and capability flags.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Concurrency contract for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolConcurrency {
    /// One call at a time; overlapping calls wait.
    #[default]
    Once,
    /// Any number of calls may run at once.
    Concurrent,
    /// Calls are queued and executed strictly in order.
    Serial,
}

/// Per-tool capability flags, surfaced in the model-facing manifest.
#[derive(Debug, Clone)]
pub struct ToolCapabilities {
    /// Concurrency contract.
    pub concurrency: ToolConcurrency,
    /// Whether the tool is side-effect free.
    pub read_only: bool,
}

impl Default for ToolCapabilities {
    fn default() -> Self {
        Self {
            concurrency: ToolConcurrency::Once,
            read_only: false,
        }
    }
}

/// The model-facing description of a tool: its name, what it does, and the
/// JSON Schema of its arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescription {
    /// Tool name, as exposed to the model.
    pub name: String,
    /// One-paragraph description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's arguments (`type: object`).
    pub parameters: Value,
}

impl ToolDescription {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

//! rh-agent — the agent definition, the model seam, and the turn/step loop.
//!
//! Fuses the two source projects on the loop level:
//!
//! * from DeepSeek Harness — the **model adapter is a plugin**: the agent
//!   resolves `dyn ModelProvider` from the [`Context`](rh_core::Context),
//!   and the session log is the source of truth for what the model sees.
//! * from Grok Build — an [`AgentBuilder`] assembles an [`Agent`] from a
//!   definition plus session context, and the loop drives one model request
//!   per step, executing the tool calls the model emits.

mod agent;
mod model;

pub use agent::{Agent, AgentBuilder, AgentDefinition, RunReport};
pub use model::{
    FinishReason, MockModelPlugin, MockModelProvider, ModelConfig, ModelEvent, ModelMessage,
    ModelProvider, ModelRequest, ModelRole, ModelStream, ModelToolCall,
};

//! The tool registry/bridge: owns tool state and routes calls.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

use rh_core::Disposer;

use crate::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolStream};

struct ToolRegistryInner {
    tools: RwLock<HashMap<ToolId, Arc<dyn Tool>>>,
}

/// The shared tool registry.
///
/// Analogous to Grok Build's `ToolBridge`: it owns the tool set and routes
/// a model's tool call to the matching [`Tool`], while each tool resolves
/// its capability services from the [`ToolCallContext`]. Cheap to clone.
#[derive(Clone)]
pub struct ToolRegistry(Arc<ToolRegistryInner>);

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self(Arc::new(ToolRegistryInner {
            tools: RwLock::new(HashMap::new()),
        }))
    }

    /// Register a tool. Returns a disposer that removes it.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Disposer {
        let id = tool.id();
        let inner = Arc::clone(&self.0);
        inner
            .tools
            .write()
            .expect("tool map poisoned")
            .insert(id.clone(), tool);
        Disposer::new(move || {
            inner.tools.write().expect("tool map poisoned").remove(&id);
        })
    }

    /// Look up a tool by id.
    pub fn get(&self, id: &ToolId) -> Option<Arc<dyn Tool>> {
        self.0
            .tools
            .read()
            .expect("tool map poisoned")
            .get(id)
            .cloned()
    }

    /// The model-facing manifest: every tool that should be listed for the
    /// given turn.
    pub fn list(&self, ctx: &ToolCallContext) -> Vec<ToolDescription> {
        self.0
            .tools
            .read()
            .expect("tool map poisoned")
            .values()
            .filter(|tool| tool.should_list(ctx))
            .map(|tool| tool.description())
            .collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.0.tools.read().expect("tool map poisoned").len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Route a model's tool call to the matching tool, returning its stream.
    pub async fn call(
        &self,
        id: &ToolId,
        ctx: &ToolCallContext,
        args: Value,
    ) -> Result<ToolStream, ToolError> {
        let tool = self.get(id).ok_or_else(|| ToolError::not_found(id.clone()))?;
        Ok(tool.execute(ctx, args).await)
    }
}

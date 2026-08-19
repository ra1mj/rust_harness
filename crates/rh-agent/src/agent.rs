//! Agent definition, builder, and the turn/step loop.

use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rh_core::Context;
use rh_session::{next_id, ContentBlock, IdKind, Session, SessionEvent};
use rh_tool::{ToolCallContext, ToolRegistry, ToolStreamItem};

use crate::model::{
    FinishReason, ModelEvent, ModelMessage, ModelProvider, ModelRequest, ModelToolCall,
};

/// A fresh tool-call id (shared with the mock provider in `model.rs`).
pub fn next_call_id() -> String {
    next_id(IdKind::ToolCall)
}

/// Everything needed to build an [`Agent`]: identity, model, system prompt,
/// tool selection, and a step budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub model: String,
    pub system_prompt: String,
    #[serde(default)]
    pub tool_ids: Vec<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
}

fn default_max_steps() -> usize {
    8
}

impl Default for AgentDefinition {
    fn default() -> Self {
        Self {
            name: "rh-agent".to_string(),
            model: "deepseek-chat".to_string(),
            system_prompt: "You are a harness agent. Call tools when useful.".to_string(),
            tool_ids: Vec::new(),
            max_steps: 8,
        }
    }
}

/// The durable transcript of a run: the session log after the turn closed.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub events: Vec<SessionEvent>,
}

/// A fully built agent: definition + session + resolved model and tools.
///
/// Immutable after construction (the Grok Build `Agent` shape): the model
/// and tool registry are resolved from the context by [`AgentBuilder`], and
/// per-turn state lives in the session log.
pub struct Agent {
    ctx: Context,
    definition: AgentDefinition,
    session: Arc<Session>,
    model: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
}

impl Agent {
    pub fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Run one turn: claim the user input, then drive zero or more steps.
    ///
    /// A **step** is one model request plus the tool calls it emits; a
    /// **turn** opens before the input is claimed and closes once nothing
    /// is owed. The model request is built exclusively from
    /// [`Session::derive_messages`], enforcing model-visible ⟺ logged.
    pub async fn run(&self, input: &str) -> anyhow::Result<RunReport> {
        let turn_id = next_id(IdKind::Turn);
        self.session.append(SessionEvent::TurnStart {
            turn_id: turn_id.clone(),
        });
        self.session.append(SessionEvent::UserMessage {
            message_id: next_id(IdKind::Message),
            content: vec![ContentBlock::Text {
                text: input.to_string(),
            }],
        });

        for _ in 0..self.definition.max_steps {
            let step_id = next_id(IdKind::Step);
            self.session.append(SessionEvent::StepStart {
                step_id: step_id.clone(),
            });

            let messages = self
                .session
                .derive_messages(Some(&self.definition.system_prompt));
            let model_messages = ModelMessage::from_session(&messages);

            let listing_ctx = ToolCallContext::new(self.ctx.clone(), self.session.id(), next_call_id())
                .with_cwd(self.session.workspace());
            let tools = self.tools.list(&listing_ctx);

            let request = ModelRequest {
                model: self.definition.model.clone(),
                messages: model_messages,
                tools,
            };

            // Consume the model stream, logging each text chunk as a durable
            // `assistant/chunk` fact so the UI can render it live, then log
            // the assembled message as the source of model history.
            let message_id = next_id(IdKind::Message);
            let mut stream = self.model.stream(request).await?;
            let mut text = String::new();
            let mut tool_calls: Vec<ModelToolCall> = Vec::new();
            let mut finish_reason = FinishReason::Stop;
            while let Some(event) = stream.next().await {
                match event {
                    ModelEvent::Text(chunk) => {
                        text.push_str(&chunk);
                        self.session.append(SessionEvent::AssistantChunk {
                            message_id: message_id.clone(),
                            text: chunk,
                        });
                        // Pace the stream so text types out visibly rather
                        // than arriving all at once.
                        tokio::time::sleep(std::time::Duration::from_millis(28)).await;
                    }
                    ModelEvent::Reasoning(chunk) => {
                        self.session.append(SessionEvent::ReasoningChunk {
                            message_id: message_id.clone(),
                            text: chunk,
                        });
                        tokio::time::sleep(std::time::Duration::from_millis(28)).await;
                    }
                    ModelEvent::ToolCall(call) => tool_calls.push(call),
                    ModelEvent::Done(reason) => finish_reason = reason,
                }
            }
            self.session.append(SessionEvent::AssistantMessage {
                message_id,
                content: assistant_blocks(&text, &tool_calls),
            });

            let wants_tools = finish_reason == FinishReason::ToolCalls && !tool_calls.is_empty();
            if !wants_tools {
                self.session.append(SessionEvent::StepEnd { step_id });
                break;
            }

            for call in &tool_calls {
                self.session.append(SessionEvent::ToolCall {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
                let call_ctx = ToolCallContext::new(self.ctx.clone(), self.session.id(), call.id.clone())
                    .with_cwd(self.session.workspace());
                let (output, is_error) = self.call_tool(&call_ctx, call).await;
                self.session.append(SessionEvent::ToolResult {
                    tool_call_id: call.id.clone(),
                    output,
                    is_error,
                });
            }
            self.session.append(SessionEvent::StepEnd { step_id });
        }

        self.session.append(SessionEvent::TurnEnd { turn_id });
        Ok(RunReport {
            events: self.session.events(),
        })
    }

    /// Execute one tool call, draining its stream to a terminal value.
    async fn call_tool(&self, ctx: &ToolCallContext, call: &ModelToolCall) -> (Value, bool) {
        match self.tools.call(&call.name, ctx, call.arguments.clone()).await {
            Ok(mut stream) => {
                let mut output: Option<Value> = None;
                let mut is_error = false;
                while let Some(item) = stream.next().await {
                    if let ToolStreamItem::Terminal(result) = item {
                        match result {
                            Ok(value) => output = Some(value),
                            Err(err) => {
                                output = Some(json!({ "error": err.to_string() }));
                                is_error = true;
                            }
                        }
                    }
                }
                (output.unwrap_or(Value::Null), is_error)
            }
            Err(err) => (json!({ "error": err.to_string() }), true),
        }
    }
}

/// Convert assembled model output into session [`ContentBlock`]s for logging.
fn assistant_blocks(text: &str, calls: &[ModelToolCall]) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }
    for call in calls {
        blocks.push(ContentBlock::ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
    }
    blocks
}

/// Assembles an [`Agent`] from a definition plus session, resolving the
/// model provider and tool registry from the shared context (the Grok Build
/// composition-root step, expressed over the `rh-core` plugin host).
pub struct AgentBuilder {
    ctx: Context,
    definition: AgentDefinition,
    model: Option<Arc<dyn ModelProvider>>,
}

impl AgentBuilder {
    pub fn new(ctx: Context, definition: AgentDefinition) -> Self {
        Self {
            ctx,
            definition,
            model: None,
        }
    }

    /// Override the model provider resolved from the context (e.g. a
    /// provider built from a runtime-selected [`ModelConfig`]).
    pub fn with_model(mut self, model: Arc<dyn ModelProvider>) -> Self {
        self.model = Some(model);
        self
    }

    pub fn build(self, session: Arc<Session>) -> anyhow::Result<Agent> {
        let model = match self.model {
            Some(model) => model,
            None => self
                .ctx
                .service::<dyn ModelProvider>()
                .ok_or_else(|| anyhow::anyhow!("no model provider registered on the context"))?,
        };
        let tools = self
            .ctx
            .service::<ToolRegistry>()
            .ok_or_else(|| anyhow::anyhow!("no tool registry registered on the context"))?;
        Ok(Agent {
            ctx: self.ctx,
            definition: self.definition,
            session,
            model,
            tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::model::{ModelRole, ModelStream};
    use rh_core::Context;
    use rh_session::{Session, SessionEvent};
    use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId, ToolRegistry};
    use serde_json::{json, Value};

    /// Test-only model provider: emits one `echo` tool call, then a final
    /// text reply after the tool result.
    struct TestProvider;

    #[async_trait]
    impl ModelProvider for TestProvider {
        async fn stream(&self, request: ModelRequest) -> anyhow::Result<ModelStream> {
            let after_tool =
                matches!(request.messages.last().map(|m| m.role), Some(ModelRole::Tool));
            let events: Vec<ModelEvent> = if after_tool {
                vec![
                    ModelEvent::Text("done".to_string()),
                    ModelEvent::Done(FinishReason::Stop),
                ]
            } else {
                vec![
                    ModelEvent::ToolCall(ModelToolCall {
                        id: "c1".to_string(),
                        name: "echo".to_string(),
                        arguments: json!({ "text": "hi" }),
                    }),
                    ModelEvent::Done(FinishReason::ToolCalls),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn id(&self) -> ToolId {
            "echo".into()
        }

        fn description(&self) -> ToolDescription {
            ToolDescription::new(
                "echo",
                "Echo the `text` argument.",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } }
                }),
            )
        }

        async fn run(&self, _ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
            Ok(args.get("text").cloned().unwrap_or(Value::Null))
        }
    }

    #[tokio::test]
    async fn agent_runs_a_turn_with_a_tool_call() {
        let ctx = Context::new();
        let registry = Arc::new(ToolRegistry::new());
        // Hold the disposers: dropping them unwinds the registrations.
        let _registry_registration = ctx.provide(registry.clone());
        let _tool_registration = registry.register(Arc::new(EchoTool));

        let provider: Arc<dyn ModelProvider> = Arc::new(TestProvider);
        let _provider_registration = ctx.provide(provider);

        let session = Session::new("s", None);
        let definition = AgentDefinition {
            name: "test".into(),
            model: "test-model".into(),
            system_prompt: "sys".into(),
            tool_ids: vec![],
            max_steps: 4,
        };
        let agent = AgentBuilder::new(ctx, definition)
            .build(session.clone())
            .unwrap();

        let report = agent.run("use the echo tool please").await.unwrap();

        assert!(report
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCall { .. })));
        assert!(report
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolResult { .. })));
        assert!(report
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnEnd { .. })));
    }
}

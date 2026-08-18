//! `workflow_step` — report the current Trellis workflow phase so the UI can
//! show a visible progress stepper.

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_session::{SessionEvent, SessionStore};
use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

/// Reports the current Trellis phase and broadcasts it as a durable event.
pub struct WorkflowStepTool;

#[async_trait]
impl Tool for WorkflowStepTool {
    fn id(&self) -> ToolId {
        "workflow_step".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "workflow_step",
            "Report the current Trellis workflow phase so progress is visible.",
            json!({
                "type": "object",
                "properties": {
                    "step": {
                        "type": "string",
                        "enum": ["brainstorm", "research", "plan", "implement", "review", "done"]
                    }
                },
                "required": ["step"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let step = args
            .get("step")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 step"))?;
        let store = ctx.service::<SessionStore>("SessionStore")?;
        let session = store
            .get(&ctx.session_id)
            .ok_or_else(|| ToolError::execution("session not found"))?;
        session.set_workflow_phase(step.to_string());
        session.append(SessionEvent::WorkflowStep {
            mode: session.work_mode(),
            step: step.to_string(),
        });
        store
            .save(&session)
            .map_err(|e| ToolError::execution(e.to_string()))?;
        Ok(json!({ "phase": step }))
    }
}

//! Skills: reusable instruction sets the agent can load on demand.
//!
//! Mirrors dsh's skill registry + grok's skill prompts. Built-in skills ship
//! in-code; user skills are `.md` files in a directory.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

/// A named, reusable instruction set.
#[derive(Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
}

/// Loads built-in skills plus user skills from a directory.
pub struct SkillStore {
    skills: Vec<Skill>,
}

impl SkillStore {
    pub fn new(dir: Option<PathBuf>) -> Arc<Self> {
        let mut skills = builtin_skills();
        if let Some(dir) = dir {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unnamed")
                            .to_string();
                        let description = content
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                            .unwrap_or_default();
                        skills.push(Skill { name, description, content });
                    }
                }
            }
        }
        Arc::new(Self { skills })
    }

    pub fn list(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.skills
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.content.clone())
    }
}

fn builtin_skills() -> Vec<Skill> {
    vec![
        Skill {
            name: "code-review".to_string(),
            description: "审查代码的 bug、风格与正确性".to_string(),
            content: "## Code Review\n\n1. Read the diff or changed files.\n2. Check for: correctness bugs, resource leaks, error handling, race conditions, style violations.\n3. Report findings as a prioritized list (critical → nit).\n4. Suggest concrete fixes for each finding.".to_string(),
        },
        Skill {
            name: "write-tests".to_string(),
            description: "为新代码编写单元测试".to_string(),
            content: "## Write Tests\n\n1. Identify the public functions/behaviors of the new code.\n2. For each, write a focused unit test covering the happy path and one edge/error case.\n3. Run the test suite and confirm green.".to_string(),
        },
        Skill {
            name: "debugging".to_string(),
            description: "系统性排查一个失败的程序".to_string(),
            content: "## Debugging\n\n1. Reproduce the failure and read the exact error.\n2. Form a hypothesis about the root cause.\n3. Add the minimal diagnostic (log / test) to confirm or refute it.\n4. Fix the root cause, not the symptom; re-run to verify.\n5. If the same fix failed twice, step back and reconsider the assumption.".to_string(),
        },
        Skill {
            name: "commit-message".to_string(),
            description: "写一条清晰的 commit message".to_string(),
            content: "## Commit Message\n\nUse `<type>: <summary>` where type ∈ {feat, fix, docs, refactor, chore, test}. Keep the summary under ~60 chars, imperative mood, no trailing period. Add a body only for non-obvious context.".to_string(),
        },
    ]
}

/// `skill` — load a skill's instructions by name.
pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn id(&self) -> ToolId {
        "skill".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "skill",
            "Load a reusable skill's instructions by name (use skill_list to see available skills).",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 name"))?;
        let store = ctx.service::<SkillStore>("SkillStore")?;
        let content = store
            .get(name)
            .ok_or_else(|| ToolError::execution(format!("技能 {name} 不存在")))?;
        Ok(json!({ "name": name, "content": content }))
    }
}

/// `skill_list` — list available skills.
pub struct SkillListTool;

#[async_trait]
impl Tool for SkillListTool {
    fn id(&self) -> ToolId {
        "skill_list".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "skill_list",
            "List available skills (name + description).",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, _args: Value) -> Result<Value, ToolError> {
        let store = ctx.service::<SkillStore>("SkillStore")?;
        let skills: Vec<Value> = store
            .list()
            .into_iter()
            .map(|(name, description)| json!({ "name": name, "description": description }))
            .collect();
        Ok(json!({ "skills": skills }))
    }
}

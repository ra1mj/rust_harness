//! Code-search tools: `grep` and `glob` (grok/opencode `grep`/`glob` analogues).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

const MAX_FILES: usize = 400;
const MAX_RESULTS: usize = 200;
const MAX_DEPTH: usize = 10;

/// Recursively search file contents for a regex pattern.
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn id(&self) -> ToolId {
        "grep".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "grep",
            "Search file contents under a directory for a regex pattern.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "regex pattern" },
                    "path": { "type": "string", "description": "directory to search (default: workspace)" }
                },
                "required": ["pattern"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 pattern 参数"))?;
        let root = args
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());
        let re = Regex::new(pattern)
            .map_err(|e| ToolError::execution(format!("无效的正则：{e}")))?;

        let mut state = WalkState::default();
        let mut results: Vec<Value> = Vec::new();
        walk(&root, &re, 0, &mut state, &mut results);
        Ok(json!({ "matches": results, "truncated": state.truncated }))
    }
}

/// List files matching a glob pattern (e.g. `**/*.rs`).
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn id(&self) -> ToolId {
        "glob".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "glob",
            "List files matching a glob pattern (e.g. `**/*.rs`, `src/*.ts`).",
            json!({
                "type": "object",
                "properties": { "pattern": { "type": "string", "description": "glob pattern" } },
                "required": ["pattern"]
            }),
        )
    }

    async fn run(&self, ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 pattern 参数"))?;
        let pat = glob::Pattern::new(pattern)
            .map_err(|e| ToolError::execution(format!("无效的 glob：{e}")))?;

        let mut state = WalkState::default();
        let mut files: Vec<String> = Vec::new();
        walk_glob(&ctx.cwd, &pat, 0, &mut state, &mut files);
        Ok(json!({ "files": files, "truncated": state.truncated }))
    }
}

#[derive(Default)]
struct WalkState {
    files_seen: usize,
    truncated: bool,
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".ref" | ".venv" | "dist" | "build"
    )
}

fn walk(dir: &Path, re: &Regex, depth: usize, state: &mut WalkState, results: &mut Vec<Value>) {
    if depth > MAX_DEPTH || state.files_seen >= MAX_FILES || results.len() >= MAX_RESULTS {
        state.truncated = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !name.starts_with('.') && !skip_dir(&name) {
                walk(&path, re, depth + 1, state, results);
            }
            continue;
        }
        state.files_seen += 1;
        if !name.starts_with('.') {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        if results.len() >= MAX_RESULTS {
                            state.truncated = true;
                            return;
                        }
                        results.push(json!({
                            "file": path.display().to_string(),
                            "line": i + 1,
                            "text": line.trim_end().chars().take(200).collect::<String>()
                        }));
                    }
                }
            }
        }
    }
}

fn walk_glob(dir: &Path, pat: &glob::Pattern, depth: usize, state: &mut WalkState, files: &mut Vec<String>) {
    if depth > MAX_DEPTH || state.files_seen >= MAX_FILES {
        state.truncated = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !name.starts_with('.') && !skip_dir(&name) {
                walk_glob(&path, pat, depth + 1, state, files);
            }
            continue;
        }
        state.files_seen += 1;
        let rel = path
            .strip_prefix(std::env::current_dir().unwrap_or_else(|_| dir.to_path_buf()))
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if pat.matches(&rel) || pat.matches(&name) {
            if files.len() >= MAX_RESULTS {
                state.truncated = true;
                return;
            }
            files.push(rel);
        }
    }
}

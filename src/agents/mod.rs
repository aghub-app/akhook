mod claude;
mod codex;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::{Value, json};

use crate::model::{Decision, ToolAttempt};

pub use claude::Claude;
pub use codex::Codex;

pub trait Agent {
    fn command(&self) -> &'static str;
    fn matcher(&self) -> &'static str;
    fn settings_file(&self, global: bool, root: &Path) -> Result<PathBuf>;
    fn decode(&self, input: &str) -> Result<Option<ToolAttempt>>;
    fn deny_json(&self, reason: &str) -> Value;

    fn format_decision(&self, decision: Decision) -> Option<Value> {
        match decision {
            Decision::Allow => None,
            Decision::Deny(hits) => Some(
                self.deny_json(
                    &hits
                        .into_iter()
                        .map(|hit| format!("akhook [{}]: {}", hit.id, hit.message))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                ),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn adapter(self) -> &'static dyn Agent {
        match self {
            Self::Claude => &Claude,
            Self::Codex => &Codex,
        }
    }
}

fn settings_path(global: bool, root: &Path, folder: &str, file: &str) -> Result<PathBuf> {
    if global {
        let home = directories::BaseDirs::new().context("cannot locate home directory")?;
        Ok(home.home_dir().join(folder).join(file))
    } else {
        Ok(root.join(folder).join(file))
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing or invalid {name}"))
}

fn event(input: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(input).context("invalid hook JSON")?;
    if field(&value, "hook_event_name")? != "PreToolUse" {
        bail!("expected PreToolUse event");
    }
    Ok(value)
}

fn cwd(value: &Value) -> Result<PathBuf> {
    let cwd = PathBuf::from(field(value, "cwd")?);
    if !cwd.is_absolute() {
        bail!("hook cwd must be absolute");
    }
    Ok(cwd)
}

fn attempt(value: &Value, cwd: PathBuf, candidates: Vec<crate::model::Candidate>) -> ToolAttempt {
    ToolAttempt {
        cwd,
        call_id: value
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        candidates,
    }
}

fn deny(reason: &str) -> Value {
    json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason
    }})
}

fn resolve(cwd: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

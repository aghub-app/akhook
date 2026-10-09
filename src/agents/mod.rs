mod adk;
mod claude;
mod codex;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::{Value, json};

use crate::{
    model::{Candidate, RuleHit, ToolAttempt},
    rules::LifecycleEvent,
};

pub use adk::Adk;
pub use claude::Claude;
pub use codex::Codex;

/// The agent's hooks that akhook handles: the agent's event name, and the
/// action of `akhook <agent> hook <action>`.
pub const HOOKS: [(&str, &str); 3] = [
    ("PreToolUse", "pre_tool_use"),
    ("UserPromptSubmit", "prompt_submit"),
    ("Stop", "stop"),
];

pub trait Agent {
    /// The agent's name on the command line.
    fn name(&self) -> &'static str;
    fn settings_file(&self, global: bool, root: &Path) -> Result<PathBuf>;
    fn decode(&self, input: &str) -> Result<Option<ToolAttempt>>;
    fn deny_json(&self, reason: &str) -> Value;
    /// The agent's own "ask the user" decision, if its hooks support one.
    fn ask_json(&self, reason: &str) -> Option<Value>;
}

/// The command an agent's settings run for a hook action.
pub fn hook_command(agent: &dyn Agent, action: &str) -> String {
    format!("akhook {} hook {action}", agent.name())
}

pub fn reason(hits: &[RuleHit]) -> String {
    hits.iter()
        .map(|hit| format!("akhook [{}]: {}", hit.id, hit.message))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AgentKind {
    Claude,
    Codex,
    /// Agents built with ADK (Agent Development Kit), whose hosts call akhook
    /// from callbacks (go/adk).
    Adk,
}

impl AgentKind {
    pub fn adapter(self) -> &'static dyn Agent {
        match self {
            Self::Claude => &Claude,
            Self::Codex => &Codex,
            Self::Adk => &Adk,
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

/// The call as a `tool_call` candidate: every tool call is one, whatever
/// else the adapter makes of it.
fn tool_call(value: &Value) -> Result<Candidate> {
    Ok(Candidate::ToolCall {
        name: field(value, "tool_name")?.into(),
        args: value.get("tool_input").cloned().unwrap_or(Value::Null),
    })
}

/// A lifecycle hook's input (UserPromptSubmit, Stop), in the shape Claude
/// Code and Codex share; each field is taken when the agent sends it.
pub fn lifecycle_event(input: &str) -> Result<LifecycleEvent> {
    let value: Value = serde_json::from_str(input).context("invalid hook JSON")?;
    let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
    Ok(LifecycleEvent {
        cwd: cwd(&value)?,
        session_id: text("session_id"),
        turn_id: text("turn_id"),
        model: text("model"),
        prompt: text("prompt"),
        last_assistant_message: text("last_assistant_message"),
    })
}

/// Context for the turn, as UserPromptSubmit's output.
pub fn context_json(context: &str) -> Value {
    json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": context
    }})
}

fn attempt(value: &Value, cwd: PathBuf, candidates: Vec<Candidate>) -> ToolAttempt {
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
    decision("deny", reason)
}

fn decision(decision: &str, reason: &str) -> Value {
    json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": decision,
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

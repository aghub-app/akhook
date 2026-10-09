use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::Value;

use super::{Agent, attempt, cwd, deny, event, tool_call};
use crate::model::ToolAttempt;

/// Agents built with ADK. ADK is a library with no hook configuration and no
/// event format of its own, so its hosts send akhook Claude Code's hook
/// format from their callbacks (go/adk). Its tools are the host's own: each
/// call is a `tool_call`, and the config's `shell_tools` says which of them
/// run shell commands. Nobody can be asked in a callback, so `ask` rules use
/// one-time approvals, as on Codex.
pub struct Adk;

impl Agent for Adk {
    fn name(&self) -> &'static str {
        "adk"
    }

    fn settings_file(&self, _global: bool, _root: &Path) -> Result<PathBuf> {
        bail!("ADK agents run akhook from their callbacks (go/adk); there is nothing to install")
    }

    fn decode(&self, input: &str) -> Result<Option<ToolAttempt>> {
        let value = event(input)?;
        let cwd = cwd(&value)?;
        let call = tool_call(&value)?;
        Ok(Some(attempt(&value, cwd, vec![call])))
    }

    fn deny_json(&self, reason: &str) -> Value {
        deny(reason)
    }

    fn ask_json(&self, _reason: &str) -> Option<Value> {
        None
    }
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Agent, attempt, cwd, deny, event, field, resolve, settings_path};
use crate::model::{Candidate, FileAction, ToolAttempt};

pub struct Claude;

impl Agent for Claude {
    fn command(&self) -> &'static str {
        "akhook claude hook pre_tool_use"
    }

    fn matcher(&self) -> &'static str {
        "^(Bash|Edit|Write)$"
    }

    fn settings_file(&self, global: bool, root: &Path) -> Result<PathBuf> {
        settings_path(global, root, ".claude", "settings.json")
    }

    fn decode(&self, input: &str) -> Result<Option<ToolAttempt>> {
        let value = event(input)?;
        let tool = field(&value, "tool_name")?;
        if !matches!(tool, "Bash" | "Edit" | "Write") {
            return Ok(None);
        }
        let cwd = cwd(&value)?;
        let payload = value.get("tool_input").context("missing tool_input")?;
        let candidate = match tool {
            "Bash" => Candidate::ShellExec {
                command: field(payload, "command")?.into(),
            },
            "Edit" => Candidate::FileChange {
                path: resolve(&cwd, field(payload, "file_path")?),
                action: FileAction::Modify,
                added_text: Some(field(payload, "new_string")?.into()),
            },
            "Write" => {
                let path = resolve(&cwd, field(payload, "file_path")?);
                let action = if path.exists() {
                    FileAction::Modify
                } else {
                    FileAction::Create
                };
                Candidate::FileChange {
                    path,
                    action,
                    added_text: Some(field(payload, "content")?.into()),
                }
            }
            _ => unreachable!(),
        };
        Ok(Some(attempt(&value, cwd, vec![candidate])))
    }

    fn deny_json(&self, reason: &str) -> Value {
        deny(reason)
    }
}

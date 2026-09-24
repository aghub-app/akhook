use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ToolAttempt {
    pub cwd: PathBuf,
    pub call_id: Option<String>,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Candidate {
    FileChange {
        path: PathBuf,
        action: FileAction,
        added_text: Option<String>,
    },
    ShellExec {
        command: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Create,
    Modify,
    Delete,
}

#[derive(Debug, Clone)]
pub struct RuleHit {
    pub id: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Allow,
    Deny(Vec<RuleHit>),
}

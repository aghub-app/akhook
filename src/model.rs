use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::RuleAction;

#[derive(Debug, Clone, Serialize)]
pub struct ToolAttempt {
    pub cwd: PathBuf,
    pub call_id: Option<String>,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub action: RuleAction,
}

/// Any denying hit denies the call; otherwise any asking hit asks the user.
#[derive(Debug, Clone)]
pub enum Decision {
    Allow,
    Deny(Vec<RuleHit>),
    Ask(Vec<RuleHit>),
}

impl Decision {
    pub fn from_hits(hits: Vec<RuleHit>) -> Self {
        if hits.is_empty() {
            Self::Allow
        } else if hits.iter().any(|hit| hit.action == RuleAction::Deny) {
            Self::Deny(
                hits.into_iter()
                    .filter(|hit| hit.action == RuleAction::Deny)
                    .collect(),
            )
        } else {
            Self::Ask(hits)
        }
    }
}

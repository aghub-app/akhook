use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Agent, attempt, cwd, deny, event, field, resolve, settings_path};
use crate::model::{Candidate, FileAction, ToolAttempt};

pub struct Codex;

impl Agent for Codex {
    fn command(&self) -> &'static str {
        "akhook codex hook pre_tool_use"
    }

    fn matcher(&self) -> &'static str {
        "^(Bash|apply_patch)$"
    }

    fn settings_file(&self, global: bool, root: &Path) -> Result<PathBuf> {
        settings_path(global, root, ".codex", "hooks.json")
    }

    fn decode(&self, input: &str) -> Result<Option<ToolAttempt>> {
        let value = event(input)?;
        let tool = field(&value, "tool_name")?;
        if !matches!(tool, "Bash" | "apply_patch") {
            return Ok(None);
        }
        let cwd = cwd(&value)?;
        let payload = value.get("tool_input").context("missing tool_input")?;
        let candidates = match tool {
            "Bash" => vec![Candidate::ShellExec {
                command: field(payload, "command")?.into(),
            }],
            "apply_patch" => parse_patch(field(payload, "command")?, &cwd)?,
            _ => unreachable!(),
        };
        Ok(Some(attempt(&value, cwd, candidates)))
    }

    fn deny_json(&self, reason: &str) -> Value {
        deny(reason)
    }
}

fn parse_patch(command: &str, cwd: &Path) -> Result<Vec<Candidate>> {
    let mut lines = command
        .lines()
        .skip_while(|line| *line != "*** Begin Patch");
    if lines.next().is_none() {
        bail!("apply_patch has no Begin Patch marker");
    }
    let mut candidates = Vec::new();
    let mut current: Option<PendingPatch> = None;
    let mut ended = false;
    for line in lines {
        if line == "*** End Patch" {
            ended = true;
            break;
        }
        let header = if let Some(path) = line.strip_prefix("*** Add File: ") {
            Some((path, FileAction::Create))
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            Some((path, FileAction::Modify))
        } else {
            line.strip_prefix("*** Delete File: ")
                .map(|path| (path, FileAction::Delete))
        };
        if let Some((path, action)) = header {
            if let Some(previous) = current.take() {
                previous.finish(&mut candidates);
            }
            if path.trim().is_empty() {
                bail!("apply_patch has empty file path");
            }
            current = Some(PendingPatch {
                path: resolve(cwd, path),
                action,
                is_add: action == FileAction::Create,
                added: Vec::new(),
                emitted: false,
            });
            continue;
        }
        let Some(pending) = &mut current else {
            bail!("apply_patch content before file header");
        };
        if let Some(destination) = line.strip_prefix("*** Move to: ") {
            if pending.action != FileAction::Modify
                || pending.emitted
                || !pending.added.is_empty()
                || destination.trim().is_empty()
            {
                bail!("invalid Move to header");
            }
            candidates.push(Candidate::FileChange {
                path: pending.path.clone(),
                action: FileAction::Delete,
                added_text: None,
            });
            pending.path = resolve(cwd, destination);
            pending.action = FileAction::Create;
        } else if let Some(text) = line.strip_prefix('+') {
            if pending.action == FileAction::Delete {
                bail!("delete patch has added lines");
            }
            pending.added.push(text.to_owned());
        } else if line.starts_with("@@")
            || line == "*** End of File"
            || line.starts_with(' ')
            || line.starts_with('-')
        {
            if pending.is_add || pending.action == FileAction::Delete {
                bail!("add patch has non-added lines");
            }
            pending.flush_added(&mut candidates);
        } else {
            bail!("unrecognized apply_patch line: {line}");
        }
    }
    if !ended {
        bail!("apply_patch has no End Patch marker");
    }
    if let Some(previous) = current {
        previous.finish(&mut candidates);
    }
    if candidates.is_empty() {
        bail!("apply_patch has no file changes");
    }
    Ok(candidates)
}

struct PendingPatch {
    path: PathBuf,
    action: FileAction,
    is_add: bool,
    added: Vec<String>,
    emitted: bool,
}

impl PendingPatch {
    fn flush_added(&mut self, candidates: &mut Vec<Candidate>) {
        if !self.added.is_empty() {
            candidates.push(Candidate::FileChange {
                path: self.path.clone(),
                action: self.action,
                added_text: Some(self.added.join("\n")),
            });
            self.added.clear();
            self.emitted = true;
        }
    }

    fn finish(mut self, candidates: &mut Vec<Candidate>) {
        self.flush_added(candidates);
        if !self.emitted {
            candidates.push(Candidate::FileChange {
                path: self.path,
                action: self.action,
                added_text: (self.action != FileAction::Delete).then(String::new),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_covers_each_file_and_rejects_malformed_input() {
        let cwd = Path::new("/tmp/project");
        let patch = "*** Begin Patch\n*** Add File: src/new.rs\n+Box::leak(x)\n*** Update File: src/old.rs\n@@\n-old\n+new\n*** Delete File: src/gone.rs\n*** End Patch";
        let items = parse_patch(patch, cwd).unwrap();
        assert_eq!(items.len(), 3);
        assert!(
            matches!(&items[0], Candidate::FileChange { action: FileAction::Create, added_text: Some(s), .. } if s == "Box::leak(x)")
        );
        assert!(matches!(
            &items[2],
            Candidate::FileChange {
                action: FileAction::Delete,
                added_text: None,
                ..
            }
        ));
        assert!(
            parse_patch(
                "*** Begin Patch\n*** Add File: x\nplain\n*** End Patch",
                cwd
            )
            .is_err()
        );
        let separated = parse_patch(
            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n+Box::\n context\n@@\n+leak(x)\n*** End Patch",
            cwd,
        )
        .unwrap();
        assert_eq!(separated.len(), 2);
        assert!(
            matches!(&separated[0], Candidate::FileChange { added_text: Some(s), .. } if s == "Box::")
        );
        assert!(
            matches!(&separated[1], Candidate::FileChange { added_text: Some(s), .. } if s == "leak(x)")
        );
    }
}

use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ast_grep_core::Pattern;
use ast_grep_language::{LanguageExt, SupportLang};
use garde::Validate;
use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::{CheckSpec, LoadedConfig, RuleEvent},
    model::{Candidate, Decision, FileAction, RuleHit, ToolAttempt},
};

#[derive(Debug, Error)]
pub enum RuleError {
    #[error("invalid rule {id}: {reason}")]
    Invalid { id: String, reason: String },
    #[error("checker for {id} failed: {reason}")]
    Checker { id: String, reason: String },
}

struct Rule {
    id: String,
    event: RuleEvent,
    paths: Vec<GlobMatcher>,
    actions: Vec<FileAction>,
    checks: Vec<Check>,
    message: String,
}

enum Check {
    Regex(Regex),
    Ast {
        language: Option<SupportLang>,
        pattern: String,
    },
    Command {
        argv: Vec<String>,
        timeout: Duration,
    },
}

pub struct RuleSet {
    root: PathBuf,
    rules: Vec<Rule>,
}

impl RuleSet {
    pub fn new(config: LoadedConfig) -> Result<Self, RuleError> {
        let rules = config
            .rules
            .into_iter()
            .map(|spec| {
                spec.validate().map_err(|error| RuleError::Invalid {
                    id: spec.id.clone(),
                    reason: error.to_string(),
                })?;
                let id = spec.id;
                let invalid = |reason: String| RuleError::Invalid {
                    id: id.clone(),
                    reason,
                };
                if spec.on == RuleEvent::ShellExec
                    && (!spec.paths.is_empty() || !spec.actions.is_empty())
                {
                    return Err(invalid("shell_exec cannot use paths or actions".into()));
                }
                let paths = spec
                    .paths
                    .into_iter()
                    .map(|glob| {
                        Glob::new(&glob)
                            .map(|g| g.compile_matcher())
                            .map_err(|e| invalid(format!("invalid path glob {glob}: {e}")))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let checks = spec
                    .checks
                    .into_iter()
                    .map(|check| match check {
                        CheckSpec::Regex { regex } => Regex::new(&regex)
                            .map(Check::Regex)
                            .map_err(|e| invalid(format!("invalid regex: {e}"))),
                        CheckSpec::Ast { ast } => {
                            if spec.on != RuleEvent::FileChange {
                                return Err(invalid("ast requires file_change".into()));
                            }
                            let language = ast
                                .language
                                .as_deref()
                                .map(str::parse::<SupportLang>)
                                .transpose()
                                .map_err(|e| invalid(format!("invalid AST language: {e}")))?;
                            if let Some(lang) = language {
                                Pattern::try_new(&ast.pattern, lang)
                                    .map_err(|e| invalid(format!("invalid AST pattern: {e}")))?;
                            }
                            Ok(Check::Ast {
                                language,
                                pattern: ast.pattern,
                            })
                        }
                        CheckSpec::Command { command } => {
                            if command.argv.is_empty()
                                || command.argv[0].is_empty()
                                || command.timeout_ms == 0
                            {
                                return Err(invalid(
                                    "command requires argv and a positive timeout_ms".into(),
                                ));
                            }
                            Ok(Check::Command {
                                argv: command.argv,
                                timeout: Duration::from_millis(command.timeout_ms),
                            })
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Rule {
                    id,
                    event: spec.on,
                    paths,
                    actions: spec.actions,
                    checks,
                    message: spec.message,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root: config.root,
            rules,
        })
    }

    pub fn evaluate(&self, attempt: &ToolAttempt) -> Result<Decision, RuleError> {
        let mut hits = Vec::new();
        let mut seen = BTreeSet::new();
        for rule in &self.rules {
            for candidate in &attempt.candidates {
                if !rule.applies(candidate, &self.root) {
                    continue;
                }
                if let Some(message) = rule.check(candidate, &self.root)? {
                    if seen.insert(rule.id.clone()) {
                        hits.push(RuleHit {
                            id: rule.id.clone(),
                            message,
                        });
                    }
                    break;
                }
            }
        }
        Ok(if hits.is_empty() {
            Decision::Allow
        } else {
            Decision::Deny(hits)
        })
    }
}

impl Rule {
    fn applies(&self, candidate: &Candidate, root: &Path) -> bool {
        match (self.event, candidate) {
            (RuleEvent::ShellExec, Candidate::ShellExec { .. }) => true,
            (RuleEvent::FileChange, Candidate::FileChange { path, action, .. }) => {
                let relative = path.strip_prefix(root).unwrap_or(path);
                let path = relative.to_string_lossy().replace('\\', "/");
                (self.paths.is_empty() || self.paths.iter().any(|glob| glob.is_match(&path)))
                    && (self.actions.is_empty() || self.actions.contains(action))
            }
            _ => false,
        }
    }

    fn check(&self, candidate: &Candidate, root: &Path) -> Result<Option<String>, RuleError> {
        for check in &self.checks {
            let matched = match (check, candidate) {
                (Check::Regex(pattern), Candidate::ShellExec { command }) => {
                    pattern.is_match(command).then_some(None)
                }
                (
                    Check::Regex(pattern),
                    Candidate::FileChange {
                        added_text: Some(text),
                        ..
                    },
                ) => pattern.is_match(text).then_some(None),
                (
                    Check::Ast { language, pattern },
                    Candidate::FileChange {
                        path,
                        added_text: Some(text),
                        ..
                    },
                ) => {
                    let lang = language.or_else(|| language_for(path)).ok_or_else(|| {
                        RuleError::Checker {
                            id: self.id.clone(),
                            reason: format!("cannot infer AST language for {}", path.display()),
                        }
                    })?;
                    let pattern =
                        Pattern::try_new(pattern, lang).map_err(|e| RuleError::Checker {
                            id: self.id.clone(),
                            reason: format!("invalid AST pattern: {e}"),
                        })?;
                    lang.ast_grep(text)
                        .root()
                        .find(pattern)
                        .is_some()
                        .then_some(None)
                }
                (Check::Command { argv, timeout }, candidate) => {
                    run_checker(&self.id, argv, *timeout, candidate, root)?
                }
                _ => None,
            };
            if let Some(dynamic) = matched {
                return Ok(Some(dynamic.unwrap_or_else(|| self.message.clone())));
            }
        }
        Ok(None)
    }
}

fn language_for(path: &Path) -> Option<SupportLang> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let name = match extension.as_str() {
        "rs" => "rust",
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" | "jsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "sh" | "bash" => "bash",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        _ => return None,
    };
    name.parse().ok()
}

#[derive(Serialize)]
struct CheckerInput<'a> {
    version: u8,
    rule_id: &'a str,
    candidate: &'a Candidate,
}

#[derive(Deserialize)]
struct CheckerOutput {
    matched: bool,
    message: Option<String>,
}

fn run_checker(
    id: &str,
    argv: &[String],
    timeout: Duration,
    candidate: &Candidate,
    root: &Path,
) -> Result<Option<Option<String>>, RuleError> {
    let error = |reason: String| RuleError::Checker {
        id: id.into(),
        reason,
    };
    let program = Path::new(&argv[0]);
    let program = if program.components().count() > 1 && program.is_relative() {
        root.join(program)
    } else {
        program.to_path_buf()
    };
    let mut child = Command::new(program)
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| error(e.to_string()))?;
    let mut normalized = candidate.clone();
    if let Candidate::FileChange { path, .. } = &mut normalized {
        *path = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    }
    let input = serde_json::to_vec(&CheckerInput {
        version: 1,
        rule_id: id,
        candidate: &normalized,
    })
    .map_err(|e| error(e.to_string()))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = thread::spawn(move || stdin.write_all(&input));
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| error(e.to_string()))? {
            Some(_) => break,
            None if started.elapsed() >= timeout => {
                child.kill().map_err(|e| error(e.to_string()))?;
                let _ = child.wait();
                let _ = writer.join();
                return Err(error("timed out".into()));
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    writer
        .join()
        .map_err(|_| error("stdin writer panicked".into()))?
        .map_err(|e| error(format!("writing stdin: {e}")))?;
    let output = child.wait_with_output().map_err(|e| error(e.to_string()))?;
    if !output.status.success() {
        return Err(error(format!(
            "exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let answer: CheckerOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| error(format!("invalid JSON output: {e}")))?;
    Ok(answer.matched.then_some(answer.message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::LoadedConfig, preset};

    #[test]
    fn complete_omp_preset_compiles_and_matches_regex_and_ast() {
        let root = PathBuf::from("/project");
        let rules = RuleSet::new(LoadedConfig {
            root: root.clone(),
            rules: preset::omp_rules().unwrap(),
        })
        .unwrap();
        let attempt = |path: &str, text: &str| ToolAttempt {
            cwd: root.clone(),
            call_id: None,
            candidates: vec![Candidate::FileChange {
                path: root.join(path),
                action: FileAction::Create,
                added_text: Some(text.into()),
            }],
        };
        let Decision::Deny(rust_hits) = rules
            .evaluate(&attempt("src/lib.rs", "Box::leak(Box::new(1))"))
            .unwrap()
        else {
            panic!("Rust regex preset did not match")
        };
        assert!(rust_hits.iter().any(|hit| hit.id == "omp/rs-box-leak"));
        let Decision::Deny(go_hits) = rules
            .evaluate(&attempt(
                "main.go",
                "func f(n int) { for i := 0; i < n; i++ { println(i) } }",
            ))
            .unwrap()
        else {
            panic!("Go AST preset did not match")
        };
        assert!(go_hits.iter().any(|hit| hit.id == "omp/go-range-int"));
        assert!(matches!(
            rules.evaluate(&attempt("src/lib.rs", "fn fine() {}")),
            Ok(Decision::Allow)
        ));
    }
}

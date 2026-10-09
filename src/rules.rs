use std::{
    collections::{BTreeMap, BTreeSet},
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
    config::{ArgvItem, CheckSpec, CommandSpec, LoadedConfig, RuleAction, RuleEvent},
    model::{Candidate, Decision, FileAction, RuleHit, ToolAttempt},
    shell,
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
    tools: Vec<GlobMatcher>,
    checks: Vec<Check>,
    message: String,
    outcome: Outcome,
}

/// How a matched rule decides: a fixed action, or a `decide` script. A
/// lifecycle rule runs its `run` command instead.
enum Outcome {
    Fixed(RuleAction),
    Script(Script),
    Run(Script),
}

struct Script {
    argv: Vec<String>,
    timeout: Duration,
}

enum Check {
    Regex(Regex),
    Argv(Vec<ArgvItem>),
    Ast {
        language: Option<SupportLang>,
        pattern: String,
    },
    Command(Script),
}

pub struct RuleSet {
    root: PathBuf,
    rules: Vec<Rule>,
    shell_tools: BTreeMap<String, String>,
    pub ask_instruction: Option<String>,
}

/// What a lifecycle rule's command gets, besides the rule and the event:
/// the fields the agent reported, each when it did.
#[derive(Debug, Default, Serialize)]
pub struct LifecycleEvent {
    pub cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// prompt_submit: the user's prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// stop: the agent's last message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
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
                if spec.on.is_lifecycle() {
                    if !spec.checks.is_empty()
                        || !spec.paths.is_empty()
                        || !spec.actions.is_empty()
                        || !spec.tools.is_empty()
                        || spec.message.is_some()
                        || spec.action.is_some()
                        || spec.decide.is_some()
                    {
                        return Err(invalid("prompt_submit and stop rules only take run".into()));
                    }
                    let run = spec
                        .run
                        .ok_or_else(|| invalid("prompt_submit and stop rules need run".into()))?;
                    return Ok(Rule {
                        id: id.clone(),
                        event: spec.on,
                        paths: Vec::new(),
                        actions: Vec::new(),
                        tools: Vec::new(),
                        checks: Vec::new(),
                        message: String::new(),
                        outcome: Outcome::Run(script(run).map_err(invalid)?),
                    });
                }
                if spec.run.is_some() {
                    return Err(invalid("run is for prompt_submit and stop rules".into()));
                }
                if spec.checks.is_empty() {
                    return Err(invalid("checks cannot be empty".into()));
                }
                let message = spec
                    .message
                    .ok_or_else(|| invalid("message is required".into()))?;
                if spec.on != RuleEvent::FileChange
                    && (!spec.paths.is_empty() || !spec.actions.is_empty())
                {
                    return Err(invalid("paths and actions require file_change".into()));
                }
                if spec.on != RuleEvent::ToolCall && !spec.tools.is_empty() {
                    return Err(invalid("tools requires tool_call".into()));
                }
                let globs = |globs: Vec<String>, what: &str| {
                    globs
                        .into_iter()
                        .map(|glob| {
                            Glob::new(&glob)
                                .map(|g| g.compile_matcher())
                                .map_err(|e| invalid(format!("invalid {what} glob {glob}: {e}")))
                        })
                        .collect::<Result<Vec<_>, _>>()
                };
                let paths = globs(spec.paths, "path")?;
                let tools = globs(spec.tools, "tool")?;
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
                        CheckSpec::Argv { argv } => {
                            if spec.on != RuleEvent::ShellExec {
                                return Err(invalid("argv requires shell_exec".into()));
                            }
                            if argv.is_empty() {
                                return Err(invalid("argv cannot be empty".into()));
                            }
                            Ok(Check::Argv(argv))
                        }
                        CheckSpec::Command { command } => {
                            script(command).map(Check::Command).map_err(invalid)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let outcome = match (spec.action, spec.decide) {
                    (Some(_), Some(_)) => {
                        return Err(invalid("action and decide cannot both be set".into()));
                    }
                    (_, Some(decide)) => Outcome::Script(script(decide).map_err(invalid)?),
                    (action, None) => Outcome::Fixed(action.unwrap_or(RuleAction::Deny)),
                };
                Ok(Rule {
                    id,
                    event: spec.on,
                    paths,
                    actions: spec.actions,
                    tools,
                    checks,
                    message,
                    outcome,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root: config.root,
            rules,
            shell_tools: config.shell_tools,
            ask_instruction: config.ask_instruction,
        })
    }

    /// The attempt's candidates, plus a `shell_exec` for each call of a tool
    /// the config names in `shell_tools`.
    fn candidates(&self, attempt: &ToolAttempt) -> Vec<Candidate> {
        let mut candidates = attempt.candidates.clone();
        for candidate in &attempt.candidates {
            if let Candidate::ToolCall { name, args } = candidate
                && let Some(command) = self
                    .shell_tools
                    .get(name)
                    .and_then(|field| args.get(field))
                    .and_then(serde_json::Value::as_str)
            {
                candidates.push(Candidate::ShellExec {
                    command: command.into(),
                });
            }
        }
        candidates
    }

    pub fn evaluate(&self, attempt: &ToolAttempt) -> Result<Decision, RuleError> {
        let mut hits = Vec::new();
        let mut seen = BTreeSet::new();
        let candidates = self.candidates(attempt);
        for rule in &self.rules {
            for candidate in &candidates {
                if !rule.applies(candidate, &self.root) {
                    continue;
                }
                let Some(message) = rule.check(candidate, &self.root)? else {
                    continue;
                };
                let (action, message) = match &rule.outcome {
                    Outcome::Fixed(action) => (*action, message),
                    Outcome::Run(_) => continue,
                    Outcome::Script(script) => {
                        let decided: DecideOutput =
                            run_script(&rule.id, script, candidate, &self.root)?;
                        let action = match decided.action {
                            DecidedAction::Allow => continue,
                            DecidedAction::Deny => RuleAction::Deny,
                            DecidedAction::Ask => RuleAction::Ask,
                        };
                        (action, decided.message.unwrap_or(message))
                    }
                };
                if seen.insert(rule.id.clone()) {
                    hits.push(RuleHit {
                        id: rule.id.clone(),
                        message,
                        action,
                    });
                }
                break;
            }
        }
        Ok(Decision::from_hits(hits))
    }

    /// Runs the event's lifecycle rules, in order, and returns the context
    /// they add. A failing command never holds up the agent: it is reported
    /// on stderr and adds nothing.
    pub fn lifecycle(&self, event: RuleEvent, data: &LifecycleEvent) -> Vec<String> {
        let mut contexts = Vec::new();
        for rule in self.rules.iter().filter(|rule| rule.event == event) {
            let Outcome::Run(script) = &rule.outcome else {
                continue;
            };
            let input = LifecycleInput {
                version: 1,
                rule_id: &rule.id,
                event,
                data,
            };
            let output = serde_json::to_vec(&input)
                .map_err(|e| RuleError::Checker {
                    id: rule.id.clone(),
                    reason: e.to_string(),
                })
                .and_then(|input| run_command(&rule.id, script, input, &self.root));
            match output {
                Ok(stdout) if stdout.iter().all(u8::is_ascii_whitespace) => {}
                Ok(stdout) => match serde_json::from_slice::<LifecycleOutput>(&stdout) {
                    Ok(LifecycleOutput {
                        context: Some(context),
                    }) if !context.trim().is_empty() => contexts.push(context),
                    Ok(_) => {}
                    Err(e) => eprintln!("akhook: {}: invalid JSON output: {e}", rule.id),
                },
                Err(error) => eprintln!("akhook: {error}"),
            }
        }
        contexts
    }
}

#[derive(Serialize)]
struct LifecycleInput<'a> {
    version: u8,
    rule_id: &'a str,
    event: RuleEvent,
    #[serde(flatten)]
    data: &'a LifecycleEvent,
}

#[derive(Deserialize)]
struct LifecycleOutput {
    context: Option<String>,
}

impl Rule {
    fn applies(&self, candidate: &Candidate, root: &Path) -> bool {
        match (self.event, candidate) {
            (RuleEvent::ShellExec, Candidate::ShellExec { .. }) => true,
            (RuleEvent::ToolCall, Candidate::ToolCall { name, .. }) => {
                self.tools.is_empty() || self.tools.iter().any(|glob| glob.is_match(name))
            }
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
                (Check::Regex(pattern), Candidate::ToolCall { args, .. }) => {
                    pattern.is_match(&args.to_string()).then_some(None)
                }
                (Check::Argv(pattern), Candidate::ShellExec { command }) => {
                    shell::simple_commands(command)
                        .iter()
                        .any(|words| shell::argv_matches(pattern, words))
                        .then_some(None)
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
                (Check::Command(script), candidate) => {
                    let answer: CheckerOutput = run_script(&self.id, script, candidate, root)?;
                    answer.matched.then_some(answer.message)
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

fn script(spec: CommandSpec) -> Result<Script, String> {
    if spec.argv.is_empty() || spec.argv[0].is_empty() || spec.timeout_ms == 0 {
        return Err("command requires argv and a positive timeout_ms".into());
    }
    Ok(Script {
        argv: spec.argv,
        timeout: Duration::from_millis(spec.timeout_ms),
    })
}

/// Output of a `decide` script.
#[derive(Deserialize)]
struct DecideOutput {
    action: DecidedAction,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecidedAction {
    Allow,
    Deny,
    Ask,
}

/// Runs a checker or decide script with the candidate on stdin and parses
/// its stdout as `T`.
fn run_script<T: serde::de::DeserializeOwned>(
    id: &str,
    script: &Script,
    candidate: &Candidate,
    root: &Path,
) -> Result<T, RuleError> {
    let error = |reason: String| RuleError::Checker {
        id: id.into(),
        reason,
    };
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
    let stdout = run_command(id, script, input, root)?;
    serde_json::from_slice(&stdout).map_err(|e| error(format!("invalid JSON output: {e}")))
}

/// Runs a rule's command from the project root with input on stdin and
/// returns its stdout; a timeout or a failed exit is an error.
fn run_command(
    id: &str,
    script: &Script,
    input: Vec<u8>,
    root: &Path,
) -> Result<Vec<u8>, RuleError> {
    let Script { argv, timeout } = script;
    let timeout = *timeout;
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
    // A command may exit without reading its input.
    match writer
        .join()
        .map_err(|_| error("stdin writer panicked".into()))?
    {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            return Err(error(format!("writing stdin: {e}")));
        }
        _ => {}
    }
    let output = child.wait_with_output().map_err(|e| error(e.to_string()))?;
    if !output.status.success() {
        return Err(error(format!(
            "exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
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
            ask_instruction: None,
            shell_tools: BTreeMap::new(),
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

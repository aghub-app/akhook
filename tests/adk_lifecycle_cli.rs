//! Tool-call rules on any agent, ADK agents, and lifecycle rules
//! (prompt_submit, stop).

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

struct Env {
    root: PathBuf,
}

impl Env {
    fn new(project: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("akhook-adk-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(root.join(".akhook.yml"), project).unwrap();
        Self { root }
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn akhook(&self, args: &[&str], stdin: &Value, env: &[(&str, &Path)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_akhook"));
        command
            .args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("AKHOOK_STATE_DIR", self.root.join("state"))
            .env_remove("AKHOOK_ADDITIONAL_CONFIG_PATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        output
    }

    /// A tool call's decision and reason, or None when it passes.
    fn call(&self, agent: &str, tool: &str, input: Value) -> Option<(String, String)> {
        let event = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": input,
            "cwd": self.root,
            "tool_use_id": "call-1",
        });
        let output = self.akhook(&[agent, "hook", "pre_tool_use"], &event, &[]);
        if output.stdout.is_empty() {
            return None;
        }
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let out = &value["hookSpecificOutput"];
        Some((
            out["permissionDecision"].as_str().unwrap().into(),
            out["permissionDecisionReason"].as_str().unwrap().into(),
        ))
    }

    fn lifecycle(&self, agent: &str, action: &str, event: Value, env: &[(&str, &Path)]) -> Output {
        let mut event = event;
        event["cwd"] = json!(self.root);
        self.akhook(&[agent, "hook", action], &event, env)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const TOOL_RULES: &str = r#"version: 1
shell_tools:
  run_shell: command
rules:
  - id: no-delete-repo
    on: tool_call
    tools: ["mcp__github__*", "delete_*"]
    checks:
      - regex: '"repo":"prod"'
    message: Leave the production repository alone.
  - id: no-force-push
    on: shell_exec
    checks:
      - argv: [git, push, --force]
    message: No force pushes.
  - id: ask-deploy
    on: tool_call
    tools: [deploy]
    action: ask
    checks:
      - regex: "."
    message: Deploys need the user's approval.
"#;

#[test]
fn tool_call_rules_match_any_agents_tools_by_name_and_arguments() {
    let env = Env::new(TOOL_RULES);
    for agent in ["claude", "codex", "adk"] {
        let (decision, reason) = env
            .call(agent, "mcp__github__delete_repo", json!({"repo": "prod"}))
            .unwrap();
        assert_eq!(decision, "deny");
        assert!(reason.contains("no-delete-repo"));
        // Other arguments, or a tool the rule does not name, pass.
        assert!(
            env.call(agent, "mcp__github__delete_repo", json!({"repo": "dev"}))
                .is_none()
        );
        assert!(
            env.call(agent, "mcp__memory__search", json!({"repo": "prod"}))
                .is_none()
        );
    }
}

#[test]
fn shell_tools_make_a_hosts_own_tool_a_shell_command() {
    let env = Env::new(TOOL_RULES);
    let (decision, reason) = env
        .call(
            "adk",
            "run_shell",
            json!({"command": "git push --force origin main"}),
        )
        .unwrap();
    assert_eq!(decision, "deny");
    assert!(reason.contains("no-force-push"));
    assert!(
        env.call("adk", "run_shell", json!({"command": "git push"}))
            .is_none()
    );
}

#[test]
fn adk_asks_through_one_time_approvals() {
    let env = Env::new(TOOL_RULES);
    let (decision, reason) = env.call("adk", "deploy", json!({"to": "prod"})).unwrap();
    assert_eq!(decision, "deny");
    assert!(reason.contains("akhook approval grant"));
    // Claude Code asks the user itself.
    let (decision, _) = env.call("claude", "deploy", json!({"to": "prod"})).unwrap();
    assert_eq!(decision, "ask");
}

#[test]
fn prompt_submit_adds_context_for_every_agent() {
    let env = Env::new("version: 1\n");
    let recall = env.script(
        "recall",
        r#"input=$(cat)
case "$input" in
  *'"event":"prompt_submit"'*'"prompt":"what is my cat called"'*) echo '{"context": "The cat is Doudou."}' ;;
  *) echo '{"context": "unexpected input"}' ;;
esac"#,
    );
    fs::write(
        env.root.join(".akhook.yml"),
        format!(
            "version: 1\nrules:\n  - id: recall\n    on: prompt_submit\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n",
            recall.display()
        ),
    )
    .unwrap();
    for agent in ["claude", "codex", "adk"] {
        let output = env.lifecycle(
            agent,
            "prompt_submit",
            json!({"hook_event_name": "UserPromptSubmit", "prompt": "what is my cat called", "turn_id": "t1"}),
            &[],
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["hookSpecificOutput"],
            json!({"hookEventName": "UserPromptSubmit", "additionalContext": "The cat is Doudou."})
        );
    }
}

#[test]
fn stop_runs_with_the_turn_and_answers_nothing() {
    let env = Env::new("version: 1\n");
    let seen = env.root.join("seen.json");
    let capture = env.script("capture", &format!("cat > {}", seen.display()));
    fs::write(
        env.root.join(".akhook.yml"),
        format!(
            "version: 1\nrules:\n  - id: capture\n    on: stop\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n",
            capture.display()
        ),
    )
    .unwrap();
    let output = env.lifecycle(
        "codex",
        "stop",
        json!({"hook_event_name": "Stop", "turn_id": "t1", "model": "gpt-x", "last_assistant_message": "Noted"}),
        &[],
    );
    assert!(output.stdout.is_empty());
    let input: Value = serde_json::from_str(&fs::read_to_string(seen).unwrap()).unwrap();
    assert_eq!(input["version"], 1);
    assert_eq!(input["rule_id"], "capture");
    assert_eq!(input["event"], "stop");
    assert_eq!(input["turn_id"], "t1");
    assert_eq!(input["model"], "gpt-x");
    assert_eq!(input["last_assistant_message"], "Noted");
    assert_eq!(input["cwd"], json!(env.root));
}

#[test]
fn failing_lifecycle_commands_never_hold_up_the_turn() {
    let env = Env::new("version: 1\n");
    let broken = env.script("broken", "echo boom >&2; exit 3");
    let slow = env.script("slow", "sleep 5");
    let fine = env.script("fine", "echo '{\"context\": \"still here\"}'");
    fs::write(
        env.root.join(".akhook.yml"),
        format!(
            "version: 1\nrules:\n  - id: a-broken\n    on: prompt_submit\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n  - id: b-slow\n    on: prompt_submit\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n      timeout_ms: 100\n  - id: c-fine\n    on: prompt_submit\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n",
            broken.display(),
            slow.display(),
            fine.display()
        ),
    )
    .unwrap();
    let output = env.lifecycle("claude", "prompt_submit", json!({"prompt": "hi"}), &[]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["hookSpecificOutput"]["additionalContext"],
        "still here"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a-broken") && stderr.contains("b-slow"),
        "{stderr}"
    );
    // A config error is reported, and the turn goes on.
    fs::write(
        env.root.join(".akhook.yml"),
        "version: 1\nrules:\n  - id: bad\n    on: stop\n",
    )
    .unwrap();
    let output = env.lifecycle("claude", "stop", json!({}), &[]);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("need run"));
}

#[test]
fn a_plugins_config_brings_its_lifecycle_rules() {
    let env = Env::new("version: 1\n");
    let hello = env.script("hello", "echo '{\"context\": \"from the plugin\"}'");
    let plugin = env.root.join("plugin-akhook.yml");
    fs::write(
        &plugin,
        format!(
            "version: 1\nrules:\n  - id: plugin/hello\n    on: prompt_submit\n    run:\n      argv: [\"/bin/sh\", \"{}\"]\n",
            hello.display()
        ),
    )
    .unwrap();
    let output = env.lifecycle(
        "adk",
        "prompt_submit",
        json!({"prompt": "hi"}),
        &[("AKHOOK_ADDITIONAL_CONFIG_PATH", &plugin)],
    );
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        value["hookSpecificOutput"]["additionalContext"],
        "from the plugin"
    );
}

#[test]
fn rule_kinds_take_only_their_own_fields() {
    let env = Env::new("version: 1\n");
    for (rule, error) in [
        (
            "on: stop\n    run: {argv: [x]}\n    checks: [{regex: x}]",
            "only take run",
        ),
        (
            "on: tool_call\n    run: {argv: [x]}\n    checks: [{regex: x}]\n    message: m",
            "run is for",
        ),
        (
            "on: shell_exec\n    tools: [x]\n    checks: [{regex: x}]\n    message: m",
            "tools requires tool_call",
        ),
        (
            "on: tool_call\n    checks: [{argv: [git]}]\n    message: m",
            "argv requires shell_exec",
        ),
        (
            "on: tool_call\n    checks: [{regex: x}]",
            "message is required",
        ),
        ("on: tool_call\n    message: m", "checks cannot be empty"),
    ] {
        fs::write(
            env.root.join(".akhook.yml"),
            format!("version: 1\nrules:\n  - id: r\n    {rule}\n"),
        )
        .unwrap();
        let (decision, reason) = env.call("claude", "anything", json!({})).unwrap();
        assert_eq!(decision, "deny");
        assert!(reason.contains(error), "{rule}: {reason}");
    }
}

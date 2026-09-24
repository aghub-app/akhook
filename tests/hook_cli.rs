use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

fn invoke(root: &Path, agent: &str, tool: &str, input: Value) -> Option<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_akhook"))
        .args([agent, "hook", "pre_tool_use"])
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let event = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
        "cwd": root,
        "tool_use_id": "test-call"
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    if output.stdout.is_empty() {
        return None;
    }
    let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
    Some(
        decision["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .into(),
    )
}

#[test]
fn both_agents_reject_matching_writes_before_execution() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("akhook-cli-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    fs::write(
        root.join(".akhook.yml"),
        "version: 1\npresets: [omp]\nrules: []\n",
    )
    .unwrap();
    let bad = "fn f() { Box::leak(Box::new(1)); }";
    let claude = invoke(
        &root,
        "claude",
        "Write",
        json!({"file_path": root.join("lib.rs"), "content": bad}),
    )
    .unwrap();
    assert!(claude.contains("omp/rs-box-leak"));
    let patch = format!(
        "*** Begin Patch\n*** Add File: safe.txt\n+ok\n*** Add File: lib.rs\n+{bad}\n*** End Patch"
    );
    let codex = invoke(&root, "codex", "apply_patch", json!({"command": patch})).unwrap();
    assert!(codex.contains("omp/rs-box-leak"));
    assert!(invoke(&root, "codex", "Bash", json!({"command": "echo ok"})).is_none());
    assert!(
        invoke(&root, "codex", "apply_patch", json!({"command": "invalid"}))
            .unwrap()
            .contains("could not check")
    );
    fs::remove_dir_all(root).unwrap();
}

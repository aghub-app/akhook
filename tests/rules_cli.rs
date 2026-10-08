use std::{
    fs,
    io::Write,
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
        let root =
            std::env::temp_dir().join(format!("akhook-rules-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(root.join(".akhook.yml"), project).unwrap();
        Self { root }
    }

    fn akhook(&self, args: &[&str], stdin: &str, env: &[(&str, &Path)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_akhook"));
        command
            .args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
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
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Runs a Bash pre-tool hook; returns (decision, reason), or None if allowed.
    fn bash(
        &self,
        agent: &str,
        command: &str,
        flags: &[&str],
        env: &[(&str, &Path)],
    ) -> Option<(String, String)> {
        let event = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": self.root,
        });
        let mut args = vec![agent, "hook", "pre_tool_use"];
        args.extend(flags);
        let output = self.akhook(&args, &event.to_string(), env);
        assert!(output.status.success());
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

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const GH_PR_CREATE: &str = "
  - id: gh-pr-create
    on: shell_exec
    checks:
      - regex: 'gh pr create'
    message: use the PR tool
";

#[test]
fn additional_configs_cannot_be_disabled_by_the_project() {
    let env = Env::new("version: 1\ndisabled_rules: [gh-pr-create]\nrules: []\n");
    let extra = env.write("extra.yml", &format!("version: 1\nrules:{GH_PR_CREATE}"));
    let flag = extra.to_str().unwrap();
    assert!(env.bash("codex", "gh pr create", &[], &[]).is_none());
    assert!(
        env.bash("codex", "gh pr create", &["--add-config-path", flag], &[])
            .is_some()
    );
    assert!(
        env.bash(
            "codex",
            "gh pr create",
            &[],
            &[("AKHOOK_ADDITIONAL_CONFIG_PATH", &extra)]
        )
        .is_some()
    );
    // A later additional config may still disable it.
    let off = env.write("off.yml", "version: 1\ndisabled_rules: [gh-pr-create]\n");
    assert!(
        env.bash(
            "codex",
            "gh pr create",
            &[
                "--add-config-path",
                flag,
                "--add-config-path",
                off.to_str().unwrap()
            ],
            &[]
        )
        .is_none()
    );
    let missing = env.root.join("missing.yml");
    let (_, reason) = env
        .bash(
            "codex",
            "echo ok",
            &["--add-config-path", missing.to_str().unwrap()],
            &[],
        )
        .unwrap();
    assert!(reason.contains("could not check"));
}

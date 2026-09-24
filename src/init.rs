use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    agents::{Agent, AgentKind},
    config,
};

const EMPTY_CONFIG: &str = "version: 1\npresets: []\nrules: []\n";

pub fn run(global: bool, mut agents: Vec<AgentKind>, cwd: &Path) -> Result<()> {
    let installed = Command::new("akhook")
        .arg("--version")
        .output()
        .context("akhook is not on PATH; install it before running init")?;
    if !installed.status.success() {
        bail!("akhook on PATH did not respond to --version");
    }
    if agents.is_empty() {
        agents = choose_agents()?;
    }
    let config_path = if global {
        config::user_config_path()?
    } else {
        cwd.join(".akhook.yml")
    };
    if !config_path.exists() {
        fs::create_dir_all(config_path.parent().context("config has no parent")?)?;
        fs::write(&config_path, EMPTY_CONFIG)
            .with_context(|| format!("writing {}", config_path.display()))?;
    }
    for kind in agents {
        let agent = kind.adapter();
        let settings = agent.settings_file(global, cwd)?;
        if !global && registered(&agent.settings_file(true, cwd)?, agent)? {
            println!(
                "{}: global hook already registered; using project rules",
                agent.command()
            );
            continue;
        }
        install(&settings, agent)?;
        println!("{}: {}", agent.command(), settings.display());
    }
    println!("rules: {}", config_path.display());
    Ok(())
}

fn choose_agents() -> Result<Vec<AgentKind>> {
    println!("Install hooks for: 1) Claude Code  2) Codex  3) Both");
    print!("Selection [3]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim() {
        "" | "3" => Ok(vec![AgentKind::Claude, AgentKind::Codex]),
        "1" => Ok(vec![AgentKind::Claude]),
        "2" => Ok(vec![AgentKind::Codex]),
        _ => bail!("invalid selection"),
    }
}

fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if !value.is_object() {
        bail!("{} must be a JSON object", path.display());
    }
    Ok(value)
}

fn has_command(value: &Value, command: &str) -> bool {
    value
        .pointer("/hooks/PreToolUse")
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command").and_then(Value::as_str) == Some(command)
                        })
                    })
            })
        })
}

fn registered(path: &Path, agent: &dyn Agent) -> Result<bool> {
    Ok(has_command(&read_settings(path)?, agent.command()))
}

fn install(path: &Path, agent: &dyn Agent) -> Result<()> {
    let mut value = read_settings(path)?;
    if has_command(&value, agent.command()) {
        return Ok(());
    }
    let object = value.as_object_mut().expect("validated object");
    let hooks = object.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().context("hooks must be an object")?;
    let groups = hooks.entry("PreToolUse").or_insert_with(|| json!([]));
    let groups = groups
        .as_array_mut()
        .context("hooks.PreToolUse must be an array")?;
    groups.push(json!({
        "matcher": agent.matcher(),
        "hooks": [{"type": "command", "command": agent.command()}]
    }));
    fs::create_dir_all(path.parent().context("settings has no parent")?)?;
    let mut output = serde_json::to_string_pretty(&value)?;
    output.push('\n');
    fs::write(path, output).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_other_hooks_and_is_idempotent() {
        let path = std::env::temp_dir().join(format!("akhook-init-{}.json", std::process::id()));
        fs::write(&path, r#"{"hooks":{"PreToolUse":[{"matcher":"Other","hooks":[{"type":"command","command":"other"}]}]}}"#).unwrap();
        install(&path, AgentKind::Codex.adapter()).unwrap();
        install(&path, AgentKind::Codex.adapter()).unwrap();
        let settings = read_settings(&path).unwrap();
        assert_eq!(
            settings
                .pointer("/hooks/PreToolUse")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(has_command(&settings, "other"));
        assert!(has_command(&settings, AgentKind::Codex.adapter().command()));
        fs::remove_file(path).unwrap();
    }
}

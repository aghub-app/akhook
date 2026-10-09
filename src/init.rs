use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    agents::{Agent, AgentKind, HOOKS, hook_command},
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
                "{}: global hooks already registered; using project rules",
                agent.name()
            );
            continue;
        }
        install(&settings, agent)?;
        println!("{}: {}", agent.name(), settings.display());
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

fn has_command(value: &Value, event: &str, command: &str) -> bool {
    value
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .is_some_and(|groups| groups.iter().any(|group| runs(group, command)))
}

fn runs(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
        })
}

fn registered(path: &Path, agent: &dyn Agent) -> Result<bool> {
    Ok(has_command(
        &read_settings(path)?,
        "PreToolUse",
        &hook_command(agent, "pre_tool_use"),
    ))
}

/// Registers akhook for each hook it handles (`HOOKS`). Tool calls are
/// registered without a matcher: the agent hands akhook all of them and the
/// adapter decides what it makes of each, so supporting a new tool needs no
/// new registration. An earlier registration limited by a matcher is widened
/// in place.
fn install(path: &Path, agent: &dyn Agent) -> Result<()> {
    let mut value = read_settings(path)?;
    let mut changed = false;
    for (event, action) in HOOKS {
        let command = hook_command(agent, action);
        let registered = has_command(&value, event, &command);
        let object = value.as_object_mut().expect("validated object");
        let hooks = object.entry("hooks").or_insert_with(|| json!({}));
        let hooks = hooks.as_object_mut().context("hooks must be an object")?;
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .with_context(|| format!("hooks.{event} must be an array"))?;
        if registered {
            for group in groups.iter_mut().filter(|group| runs(group, &command)) {
                if let Some(group) = group.as_object_mut() {
                    changed |= group.remove("matcher").is_some();
                }
            }
        } else {
            groups.push(json!({
                "hooks": [{"type": "command", "command": command}]
            }));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
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
        assert!(has_command(&settings, "PreToolUse", "other"));
        let codex = AgentKind::Codex.adapter();
        for (event, action) in HOOKS {
            assert!(has_command(&settings, event, &hook_command(codex, action)));
        }
        // Every tool call reaches akhook.
        assert!(settings.pointer("/hooks/PreToolUse/1/matcher").is_none());
        // Other hooks keep their matchers.
        assert_eq!(
            settings.pointer("/hooks/PreToolUse/0/matcher").unwrap(),
            "Other"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn widens_a_registration_limited_by_a_matcher() {
        let path =
            std::env::temp_dir().join(format!("akhook-init-old-{}.json", std::process::id()));
        let command = hook_command(AgentKind::Claude.adapter(), "pre_tool_use");
        fs::write(
            &path,
            json!({"hooks": {"PreToolUse": [
                {"matcher": "^(Bash|Edit|Write)$", "hooks": [{"type": "command", "command": command}]}
            ]}})
            .to_string(),
        )
        .unwrap();
        install(&path, AgentKind::Claude.adapter()).unwrap();
        let settings = read_settings(&path).unwrap();
        let groups = settings
            .pointer("/hooks/PreToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].get("matcher").is_none());
        fs::remove_file(path).unwrap();
    }
}

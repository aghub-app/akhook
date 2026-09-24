use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use garde::Validate;
use serde::Deserialize;

use crate::{model::FileAction, preset};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u8,
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub disabled_rules: Vec<String>,
    #[serde(default)]
    pub rules: Vec<RuleSpec>,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    #[garde(custom(non_blank))]
    pub id: String,
    #[garde(skip)]
    pub on: RuleEvent,
    #[serde(default)]
    #[garde(skip)]
    pub paths: Vec<String>,
    #[serde(default)]
    #[garde(skip)]
    pub actions: Vec<FileAction>,
    #[garde(length(min = 1))]
    pub checks: Vec<CheckSpec>,
    #[garde(custom(non_blank))]
    pub message: String,
}

fn non_blank(value: &str, _: &()) -> garde::Result {
    if value.trim().is_empty() {
        Err(garde::Error::new("must not be blank"))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEvent {
    FileChange,
    ShellExec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CheckSpec {
    Regex { regex: String },
    Ast { ast: AstSpec },
    Command { command: CommandSpec },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AstSpec {
    pub language: Option<String>,
    pub pattern: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_timeout() -> u64 {
    5_000
}

pub struct LoadedConfig {
    pub root: PathBuf,
    pub rules: Vec<RuleSpec>,
}

pub fn user_config_path() -> Result<PathBuf> {
    let dirs = BaseDirs::new().context("cannot locate user config directory")?;
    Ok(dirs.config_dir().join("akhook/akhook.yml"))
}

pub fn project_config_path(cwd: &Path) -> Result<Option<PathBuf>> {
    for path in cwd.ancestors().map(|dir| dir.join(".akhook.yml")) {
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => return Ok(Some(path)),
            Ok(_) => bail!("{} is not a file", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
    }
    Ok(None)
}

fn read_config(path: &Path) -> Result<Option<Config>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let config: Config =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if config.version != 1 {
        bail!(
            "unsupported config version {} in {}",
            config.version,
            path.display()
        );
    }
    Ok(Some(config))
}

pub fn load(cwd: &Path) -> Result<LoadedConfig> {
    let project_path = project_config_path(cwd)?;
    let root = project_path
        .as_ref()
        .and_then(|p| p.parent())
        .unwrap_or(cwd)
        .to_path_buf();
    let global = read_config(&user_config_path()?)?;
    let project = project_path
        .as_deref()
        .map(read_config)
        .transpose()?
        .flatten();
    let mut by_id = BTreeMap::new();
    let mut disabled = Vec::new();
    let use_omp = global
        .as_ref()
        .is_some_and(|c| c.presets.iter().any(|p| p == "omp"))
        || project
            .as_ref()
            .is_some_and(|c| c.presets.iter().any(|p| p == "omp"));
    for config in [global.as_ref(), project.as_ref()].into_iter().flatten() {
        for name in &config.presets {
            if name != "omp" {
                bail!("unknown preset {name}");
            }
        }
    }
    if use_omp {
        for rule in preset::omp_rules()? {
            by_id.insert(rule.id.clone(), rule);
        }
    }
    for config in [global, project].into_iter().flatten() {
        disabled.extend(config.disabled_rules);
        for rule in config.rules {
            if rule.id.trim().is_empty() {
                bail!("rule id cannot be empty");
            }
            by_id.insert(rule.id.clone(), rule);
        }
    }
    for id in disabled {
        by_id.remove(&id);
    }
    Ok(LoadedConfig {
        root,
        rules: by_id.into_values().collect(),
    })
}

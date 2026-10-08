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
    #[serde(default)]
    pub ask_instruction: Option<String>,
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
    #[serde(default)]
    #[garde(skip)]
    pub action: Option<RuleAction>,
}

/// What a matched rule does. Without `action` it denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Deny,
    Ask,
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
    Argv { argv: Vec<ArgvItem> },
    Command { command: CommandSpec },
}

/// One position of an `argv` check: a word, or any of several words.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgvItem {
    One(String),
    Any(Vec<String>),
}

impl ArgvItem {
    pub fn matches(&self, word: &str) -> bool {
        match self {
            Self::One(expected) => expected == word,
            Self::Any(choices) => choices.iter().any(|choice| choice == word),
        }
    }
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
    pub ask_instruction: Option<String>,
}

/// Additional configs named by `AKHOOK_ADDITIONAL_CONFIG_PATH` (a path list)
/// followed by `--add-config-path` flags; later ones take precedence.
pub fn additional_config_paths(flags: Vec<PathBuf>) -> Vec<PathBuf> {
    std::env::var_os(ADDITIONAL_CONFIG_ENV)
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .chain(flags)
        .collect()
}

pub const ADDITIONAL_CONFIG_ENV: &str = "AKHOOK_ADDITIONAL_CONFIG_PATH";

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

/// Merges preset, user and project rules (`disabled_rules` applies to all of
/// them), then each additional config in order. An additional config's rules
/// cannot be disabled or replaced by the layers before it, only by itself or
/// a later additional config.
pub fn load(cwd: &Path, additional: &[PathBuf]) -> Result<LoadedConfig> {
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
    let additional = additional
        .iter()
        .map(|path| {
            read_config(path)?
                .with_context(|| format!("additional config {} not found", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let layers = [global.as_ref(), project.as_ref()]
        .into_iter()
        .flatten()
        .chain(&additional);
    let mut use_omp = false;
    let mut ask_instruction = None;
    for config in layers {
        for name in &config.presets {
            if name != "omp" {
                bail!("unknown preset {name}");
            }
            use_omp = true;
        }
        if config.ask_instruction.is_some() {
            ask_instruction.clone_from(&config.ask_instruction);
        }
    }
    let mut by_id = BTreeMap::new();
    if use_omp {
        for rule in preset::omp_rules()? {
            by_id.insert(rule.id.clone(), rule);
        }
    }
    let mut disabled = Vec::new();
    for config in [global, project].into_iter().flatten() {
        disabled.extend(config.disabled_rules);
        insert_rules(&mut by_id, config.rules)?;
    }
    for id in disabled {
        by_id.remove(&id);
    }
    for config in additional {
        for id in &config.disabled_rules {
            by_id.remove(id);
        }
        insert_rules(&mut by_id, config.rules)?;
    }
    Ok(LoadedConfig {
        root,
        rules: by_id.into_values().collect(),
        ask_instruction,
    })
}

fn insert_rules(by_id: &mut BTreeMap<String, RuleSpec>, rules: Vec<RuleSpec>) -> Result<()> {
    for rule in rules {
        if rule.id.trim().is_empty() {
            bail!("rule id cannot be empty");
        }
        by_id.insert(rule.id.clone(), rule);
    }
    Ok(())
}

mod agents;
mod config;
mod init;
mod model;
mod preset;
mod rules;

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    agents::{Agent, AgentKind},
    rules::RuleSet,
};

#[derive(Parser)]
#[command(version, about = "Cross-agent hook rule engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Extra config applied after the user and project configs; repeatable.
    /// Also read from AKHOOK_ADDITIONAL_CONFIG_PATH (a path list).
    #[arg(long = "add-config-path", global = true, value_name = "PATH")]
    add_config_path: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        global: bool,
        #[arg(long, value_enum)]
        agent: Vec<AgentKind>,
    },
    Claude {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Codex {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Subcommand)]
enum HookAction {
    #[command(name = "pre_tool_use")]
    PreToolUse,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("akhook: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let additional = config::additional_config_paths(cli.add_config_path);
    match cli.command {
        Command::Init { global, agent } => {
            init::run(global, agent, Path::new(".").canonicalize()?.as_path())
        }
        Command::Claude { command } => hook(AgentKind::Claude.adapter(), command, &additional),
        Command::Codex { command } => hook(AgentKind::Codex.adapter(), command, &additional),
    }
}

fn hook(agent: &dyn Agent, command: AgentCommand, additional: &[PathBuf]) -> Result<()> {
    let AgentCommand::Hook {
        action: HookAction::PreToolUse,
    } = command;
    let result = (|| {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("reading hook input")?;
        let Some(attempt) = agent.decode(&input)? else {
            return Ok(None);
        };
        let rules = RuleSet::new(config::load(&attempt.cwd, additional)?)?;
        Ok::<_, anyhow::Error>(agent.format_decision(rules.evaluate(&attempt)?))
    })();
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            eprintln!("akhook: {error:#}");
            Some(agent.deny_json(&format!("akhook could not check this tool call: {error:#}")))
        }
    };
    if let Some(response) = response {
        println!("{}", serde_json::to_string(&response)?);
    }
    Ok(())
}

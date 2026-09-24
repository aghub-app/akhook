mod agents;
mod config;
mod init;
mod model;
mod preset;
mod rules;

use std::{
    io::{self, Read},
    path::Path,
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
    match cli.command {
        Command::Init { global, agent } => {
            init::run(global, agent, Path::new(".").canonicalize()?.as_path())
        }
        Command::Claude { command } => hook(AgentKind::Claude.adapter(), command),
        Command::Codex { command } => hook(AgentKind::Codex.adapter(), command),
    }
}

fn hook(agent: &dyn Agent, command: AgentCommand) -> Result<()> {
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
        let rules = RuleSet::new(config::load(&attempt.cwd)?)?;
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

mod agents;
mod approval;
mod config;
mod init;
mod model;
mod preset;
mod rules;
mod shell;

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    agents::{Agent, AgentKind},
    model::Decision,
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
    /// One-time approvals for `ask` rules on agents that cannot ask (Codex).
    Approval {
        #[command(subcommand)]
        command: ApprovalCommand,
    },
}

#[derive(Subcommand)]
enum ApprovalCommand {
    /// Print a pending request as JSON.
    Show { id: String },
    /// Let the requested call through once, within ten minutes.
    Grant { id: String },
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
        Command::Approval {
            command: ApprovalCommand::Show { id },
        } => {
            println!("{}", serde_json::to_string_pretty(&approval::show(&id)?)?);
            Ok(())
        }
        Command::Approval {
            command: ApprovalCommand::Grant { id },
        } => approval::grant(&id),
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
        let response = match rules.evaluate(&attempt)? {
            Decision::Allow => None,
            Decision::Deny(hits) => Some(agent.deny_json(&agents::reason(&hits))),
            Decision::Ask(hits) => {
                let reason = agents::reason(&hits);
                match agent.ask_json(&reason) {
                    Some(ask) => Some(ask),
                    None => {
                        match approval::check(&attempt, &hits, rules.ask_instruction.as_deref())? {
                            approval::Outcome::Granted => None,
                            approval::Outcome::Requested(instruction) => {
                                Some(agent.deny_json(&format!("{reason}\n\n{instruction}")))
                            }
                        }
                    }
                }
            }
        };
        Ok::<_, anyhow::Error>(response)
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

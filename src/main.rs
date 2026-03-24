use std::process;

use agent_playground::{
    config::{AppConfig, init_playground},
    listing::list_playgrounds,
    runner::run_playground,
};
use anyhow::{Context, Result};
use clap::{Arg, ArgAction, Command};

fn build_cli() -> Command {
    Command::new("agent-playground")
        .about("A minimal CLI for running agent in playground")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("init")
                .about("Initialize config for a playground")
                .arg(
                    Arg::new("playground_id")
                        .value_name("PLAYGROUND_ID")
                        .help("The playground identifier to initialize")
                        .required(true),
                )
                .arg(
                    Arg::new("agent_ids")
                        .long("agent")
                        .value_name("AGENT_ID")
                        .help(
                            "Initialize the template config directory for an agent. Repeat to include multiple agents.",
                        )
                        .action(ArgAction::Append),
                ),
        )
        .subcommand(Command::new("list").about("List all playgrounds"))
        .arg(
            Arg::new("playground_id")
                .value_name("PLAYGROUND_ID")
                .help("The playground id to play in")
                .required(false),
        )
        .arg(
            Arg::new("agent_id")
                .long("agent")
                .value_name("AGENT_ID")
                .help("The agent identifier to use for this run")
                .required(false),
        )
}

fn main() -> Result<()> {
    let matches = build_cli().get_matches();

    if let Some(("init", init_matches)) = matches.subcommand() {
        let playground_id = init_matches
            .get_one::<String>("playground_id")
            .expect("required by clap");
        let selected_agent_ids = init_matches
            .get_many::<String>("agent_ids")
            .map(|values| values.cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let result = init_playground(playground_id, &selected_agent_ids)?;

        println!(
            "initialized playground '{}' in {}",
            result.playground_id,
            result
                .paths
                .playgrounds_dir
                .join(&result.playground_id)
                .display()
        );
        if !result.initialized_agent_templates.is_empty() {
            println!(
                "initialized agent config templates: {}",
                result.initialized_agent_templates.join(", ")
            );
        }
        return Ok(());
    }

    if let Some(("list", _)) = matches.subcommand() {
        list_playgrounds()?;
        return Ok(());
    }

    let config = AppConfig::load()?;
    let exit_code = run_playground(
        &config,
        matches
            .get_one::<String>("playground_id")
            .context("missing playground_id")?,
        matches.get_one::<String>("agent_id").map(String::as_str),
    )?;

    process::exit(exit_code);
}

use std::process;

use agent_playground::{
    config::{AppConfig, init_playground},
    listing::list_playgrounds,
    runner::run_playground,
};
use anyhow::{Context, Result};
use clap::{Arg, Command};

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
        let result = init_playground(playground_id)?;

        println!(
            "initialized playground '{}' in {}",
            result.playground_id,
            result
                .paths
                .playgrounds_dir
                .join(&result.playground_id)
                .display()
        );
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

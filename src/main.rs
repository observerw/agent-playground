use std::process;

use agent_playground::{
    config::{AppConfig, init_playground},
    listing::list_playgrounds,
    runner::run_playground,
};
use anyhow::{Context, Result};
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "agent-playground",
    about = "A minimal CLI for running agent in playground",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(
        value_name = "PLAYGROUND_ID",
        help = "The playground id to play in",
        required = false
    )]
    playground_id: Option<String>,
    #[arg(
        long = "agent",
        value_name = "AGENT_ID",
        help = "The agent identifier to use for this run",
        required = false
    )]
    agent_id: Option<String>,
    #[arg(
        long = "save",
        help = "Save the temporary playground snapshot on normal exit",
        action = ArgAction::SetTrue
    )]
    save: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize config for a playground
    Init(InitArgs),
    /// List all playgrounds
    List,
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(
        value_name = "PLAYGROUND_ID",
        help = "The playground identifier to initialize"
    )]
    playground_id: String,
    #[arg(
        long = "agent",
        value_name = "AGENT_ID",
        help = "Initialize the template config directory for an agent. Repeat to include multiple agents.",
        action = ArgAction::Append
    )]
    agent_ids: Vec<String>,
}

fn build_cli() -> clap::Command {
    Cli::command()
}

fn handle_init(args: InitArgs) -> Result<()> {
    let result = init_playground(&args.playground_id, &args.agent_ids)?;

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

    Ok(())
}

fn handle_run(cli: Cli) -> Result<()> {
    let config = AppConfig::load()?;
    let exit_code = run_playground(
        &config,
        cli.playground_id
            .as_deref()
            .context("missing playground_id")?,
        cli.agent_id.as_deref(),
        cli.save,
    )?;

    process::exit(exit_code);
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init(args)) => handle_init(args),
        Some(Commands::List) => {
            list_playgrounds()?;
            Ok(())
        }
        None => handle_run(cli),
    }
}

#[cfg(test)]
mod tests {
    use super::build_cli;

    #[test]
    fn run_command_does_not_save_by_default() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "demo"])
            .expect("cli should parse");

        assert_eq!(matches.get_one::<bool>("save"), Some(&false));
    }

    #[test]
    fn run_command_accepts_explicit_save_flag() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "demo", "--save"])
            .expect("cli should parse");

        assert_eq!(matches.get_one::<bool>("save"), Some(&true));
    }

    #[test]
    fn run_command_rejects_save_with_explicit_value() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "demo", "--save=false"])
            .expect_err("cli should reject value for save flag");

        assert_eq!(matches.kind(), clap::error::ErrorKind::TooManyValues);
    }

    #[test]
    fn init_subcommand_parses_playground_and_agents() {
        let matches = build_cli()
            .try_get_matches_from([
                "apg", "init", "demo", "--agent", "claude", "--agent", "codex",
            ])
            .expect("cli should parse");

        let Some(("init", init_matches)) = matches.subcommand() else {
            panic!("init subcommand")
        };
        assert_eq!(
            init_matches.get_one::<String>("playground_id"),
            Some(&"demo".to_string())
        );
        assert_eq!(
            init_matches
                .get_many::<String>("agent_ids")
                .expect("agent ids")
                .cloned()
                .collect::<Vec<_>>(),
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn list_subcommand_parses_without_run_arguments() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "list"])
            .expect("cli should parse");

        assert!(matches.subcommand_matches("list").is_some());
        assert!(matches.get_one::<String>("playground_id").is_none());
    }

    #[test]
    fn root_command_requires_playground_or_subcommand() {
        let error = build_cli()
            .try_get_matches_from(["apg"])
            .expect_err("cli should reject empty input");

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }
}

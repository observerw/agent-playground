use std::process;

use agent_playground::{
    config::{AppConfig, init_playground},
    listing::list_playgrounds,
    runner::run_playground,
};
use anyhow::{Context, Result};
use clap::builder::BoolishValueParser;
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
        .arg(
            Arg::new("save")
                .long("save")
                .value_name("BOOL")
                .help("Save the temporary playground snapshot on normal exit")
                .action(ArgAction::Set)
                .num_args(0..=1)
                .default_value("false")
                .default_missing_value("true")
                .value_parser(BoolishValueParser::new()),
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
        *matches.get_one::<bool>("save").unwrap_or(&false),
    )?;

    process::exit(exit_code);
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
    fn run_command_allows_disabling_save() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "demo", "--save=false"])
            .expect("cli should parse");

        assert_eq!(matches.get_one::<bool>("save"), Some(&false));
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

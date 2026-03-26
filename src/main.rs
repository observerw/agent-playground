use std::{
    io::{self, BufRead, Write},
    path::Path,
    process,
};

use agent_playground::{
    config::{AppConfig, init_playground, remove_playground, resolve_playground_dir},
    info::show_playground_info,
    listing::list_playgrounds,
    runner::{DirectoryMount, run_default_playground, run_playground},
};
use anyhow::{Context, Result};
use clap::{ArgAction, Args, Parser, Subcommand};

#[cfg(test)]
use clap::CommandFactory;

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
    #[arg(
        long = "with",
        value_name = "SOURCE[:RELATIVE_DESTINATION]",
        help = "Symlink-mount an external directory into the temporary playground. Repeat to mount multiple directories.",
        action = ArgAction::Append
    )]
    mounts: Vec<DirectoryMount>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Launch an agent in an empty temporary playground (defaults to the configured default agent when `--agent` is not provided)
    Default(DefaultArgs),
    /// Initialize config for a playground
    Init(InitArgs),
    /// Show detailed information for a playground
    Info(InfoArgs),
    /// List all playgrounds
    List,
    /// Print the absolute path for a playground template directory
    Path(PathArgs),
    /// Remove a playground from the global config directory
    Remove(RemoveArgs),
}

#[derive(Debug, Args)]
struct DefaultArgs {
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
    #[arg(
        long = "with",
        value_name = "SOURCE[:RELATIVE_DESTINATION]",
        help = "Symlink-mount an external directory into the temporary playground. Repeat to mount multiple directories.",
        action = ArgAction::Append
    )]
    mounts: Vec<DirectoryMount>,
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

#[derive(Debug, Args)]
struct InfoArgs {
    #[arg(
        value_name = "PLAYGROUND_ID",
        help = "The playground identifier to inspect"
    )]
    playground_id: String,
}

#[derive(Debug, Args)]
struct PathArgs {
    #[arg(
        value_name = "PLAYGROUND_ID",
        help = "The playground identifier whose path should be printed"
    )]
    playground_id: String,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    #[arg(
        value_name = "PLAYGROUND_ID",
        help = "The playground identifier to remove"
    )]
    playground_id: String,
    #[arg(
        short = 'y',
        long = "yes",
        help = "Skip confirmation prompt",
        action = ArgAction::SetTrue
    )]
    yes: bool,
}

#[cfg(test)]
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
        &cli.mounts,
    )?;

    process::exit(exit_code);
}

fn handle_default(args: DefaultArgs) -> Result<()> {
    let config = AppConfig::load()?;
    let exit_code =
        run_default_playground(&config, args.agent_id.as_deref(), args.save, &args.mounts)?;

    process::exit(exit_code);
}

fn handle_path(args: PathArgs) -> Result<()> {
    println!("{}", resolve_playground_dir(&args.playground_id)?.display());
    Ok(())
}

fn prompt_to_remove_playground<R: BufRead, W: Write>(
    playground_id: &str,
    playground_dir: &Path,
    mut input: R,
    output: &mut W,
) -> Result<bool> {
    write!(
        output,
        "Remove playground '{}' from {}? [y/N] ",
        playground_id,
        playground_dir.display()
    )?;
    output.flush()?;

    let mut response = String::new();
    input.read_line(&mut response)?;

    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn handle_remove(args: RemoveArgs) -> Result<()> {
    let playground_dir = resolve_playground_dir(&args.playground_id)?;

    if !args.yes
        && !prompt_to_remove_playground(
            &args.playground_id,
            &playground_dir,
            io::stdin().lock(),
            &mut io::stdout().lock(),
        )?
    {
        println!("aborted removing playground '{}'", args.playground_id);
        return Ok(());
    }

    let result = remove_playground(&args.playground_id)?;
    println!(
        "removed playground '{}' from {}",
        result.playground_id,
        result.playground_dir.display()
    );

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Default(args)) => handle_default(args),
        Some(Commands::Init(args)) => handle_init(args),
        Some(Commands::Info(args)) => show_playground_info(&args.playground_id),
        Some(Commands::List) => {
            list_playgrounds()?;
            Ok(())
        }
        Some(Commands::Path(args)) => handle_path(args),
        Some(Commands::Remove(args)) => handle_remove(args),
        None => handle_run(cli),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use agent_playground::runner::DirectoryMount;

    use super::{build_cli, prompt_to_remove_playground};
    use tempfile::tempdir;

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
    fn default_subcommand_parses_agent_and_save_flag() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("shared");
        fs::create_dir_all(&source).expect("create mount source");

        let matches = build_cli()
            .try_get_matches_from([
                "apg",
                "default",
                "--agent",
                "codex",
                "--save",
                "--with",
                &format!("{}:tools/shared", source.display()),
            ])
            .expect("cli should parse");

        let Some(("default", default_matches)) = matches.subcommand() else {
            panic!("default subcommand")
        };
        assert_eq!(
            default_matches.get_one::<String>("agent_id"),
            Some(&"codex".to_string())
        );
        assert_eq!(default_matches.get_one::<bool>("save"), Some(&true));
        assert_eq!(
            default_matches
                .get_many::<DirectoryMount>("mounts")
                .expect("mounts")
                .count(),
            1
        );
    }

    #[test]
    fn run_command_parses_repeated_mount_flags() {
        let temp = tempdir().expect("tempdir");
        let source_a = temp.path().join("alpha");
        let source_b = temp.path().join("beta");
        fs::create_dir_all(&source_a).expect("create alpha");
        fs::create_dir_all(&source_b).expect("create beta");

        let matches = build_cli()
            .try_get_matches_from([
                "apg",
                "demo",
                "--with",
                &source_a.display().to_string(),
                "--with",
                &format!("{}:nested/beta", source_b.display()),
            ])
            .expect("cli should parse");

        let mounts = matches
            .get_many::<DirectoryMount>("mounts")
            .expect("mounts")
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].destination, Path::new("alpha"));
        assert_eq!(mounts[1].destination, Path::new("nested/beta"));
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
    fn info_subcommand_parses_playground_id() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "info", "demo"])
            .expect("cli should parse");

        let Some(("info", info_matches)) = matches.subcommand() else {
            panic!("info subcommand")
        };
        assert_eq!(
            info_matches.get_one::<String>("playground_id"),
            Some(&"demo".to_string())
        );
    }

    #[test]
    fn path_subcommand_parses_playground_id() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "path", "demo"])
            .expect("cli should parse");

        let Some(("path", path_matches)) = matches.subcommand() else {
            panic!("path subcommand")
        };
        assert_eq!(
            path_matches.get_one::<String>("playground_id"),
            Some(&"demo".to_string())
        );
    }

    #[test]
    fn remove_subcommand_parses_playground_and_yes_flag() {
        let matches = build_cli()
            .try_get_matches_from(["apg", "remove", "demo", "-y"])
            .expect("cli should parse");

        let Some(("remove", remove_matches)) = matches.subcommand() else {
            panic!("remove subcommand")
        };
        assert_eq!(
            remove_matches.get_one::<String>("playground_id"),
            Some(&"demo".to_string())
        );
        assert_eq!(remove_matches.get_one::<bool>("yes"), Some(&true));
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

    #[test]
    fn remove_prompt_accepts_yes_and_rejects_default_enter() {
        let mut output = Vec::new();
        let accepted = prompt_to_remove_playground(
            "demo",
            Path::new("/tmp/demo"),
            std::io::Cursor::new("yes\n"),
            &mut output,
        )
        .expect("prompt should succeed");

        assert!(accepted);
        assert_eq!(
            String::from_utf8(output).expect("utf8 output"),
            "Remove playground 'demo' from /tmp/demo? [y/N] "
        );

        let mut output = Vec::new();
        let accepted = prompt_to_remove_playground(
            "demo",
            Path::new("/tmp/demo"),
            std::io::Cursor::new("\n"),
            &mut output,
        )
        .expect("prompt should succeed");

        assert!(!accepted);
    }
}

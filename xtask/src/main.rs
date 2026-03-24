use std::path::PathBuf;

use agent_playground::schema::{default_schema_site_dir, write_schema_site};
use anyhow::Result;
use clap::{Arg, Command};

fn build_cli() -> Command {
    Command::new("xtask")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("generate-schema-site")
                .about("Generate JSON Schema files and a GitHub Pages site")
                .arg(
                    Arg::new("output_dir")
                        .long("output-dir")
                        .value_name("PATH")
                        .help("Directory where the schema site will be written")
                        .required(false)
                        .default_value("target/schema-site"),
                ),
        )
}

fn main() -> Result<()> {
    let matches = build_cli().get_matches();

    if let Some(("generate-schema-site", generate_matches)) = matches.subcommand() {
        let output_dir = generate_matches
            .get_one::<String>("output_dir")
            .map(PathBuf::from)
            .unwrap_or_else(default_schema_site_dir);

        write_schema_site(&output_dir)?;
        println!("generated schema site in {}", output_dir.display());
    }

    Ok(())
}

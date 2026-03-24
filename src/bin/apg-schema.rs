use std::path::PathBuf;

use agent_playground::schema::{default_schema_site_dir, write_schema_site};
use anyhow::Result;
use clap::{Arg, Command};

fn build_cli() -> Command {
    Command::new("apg-schema")
        .about("Generate JSON Schema files and a GitHub Pages site")
        .arg(
            Arg::new("output_dir")
                .long("output-dir")
                .value_name("PATH")
                .help("Directory where the schema site will be written")
                .required(false)
                .default_value("target/schema-site"),
        )
}

fn main() -> Result<()> {
    let matches = build_cli().get_matches();
    let output_dir = matches
        .get_one::<String>("output_dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_schema_site_dir);

    write_schema_site(&output_dir)?;
    println!("generated schema site in {}", output_dir.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_cli;

    #[test]
    fn uses_default_output_dir() {
        let matches = build_cli()
            .try_get_matches_from(["apg-schema"])
            .expect("cli should parse");

        assert_eq!(
            matches.get_one::<String>("output_dir"),
            Some(&"target/schema-site".to_string())
        );
    }
}

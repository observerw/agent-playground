//! Utilities for listing configured playgrounds in a human-readable table.
//!
//! This module is used by the CLI `list` subcommand and focuses on
//! presentation-oriented output.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::config::{AppConfig, PlaygroundDefinition};

/// Loads application configuration and prints all configured playgrounds.
///
/// The output is a simple fixed-width table with `PLAYGROUND` and
/// `DESCRIPTION` columns. If no playgrounds are configured, a fallback
/// message is printed instead.
///
/// # Errors
///
/// Returns an error when configuration loading fails.
pub fn list_playgrounds() -> Result<()> {
    let config = AppConfig::load()?;
    print!("{}", format_playgrounds(&config.playgrounds));
    Ok(())
}

fn format_playgrounds(playgrounds: &BTreeMap<String, PlaygroundDefinition>) -> String {
    if playgrounds.is_empty() {
        return "No playgrounds found.\n".to_string();
    }

    let id_width = playgrounds
        .keys()
        .map(|id| id.len())
        .max()
        .unwrap_or("PLAYGROUND".len())
        .max("PLAYGROUND".len());

    let mut output = String::new();
    output.push_str(&format!(
        "{:<id_width$}  {}\n",
        "PLAYGROUND",
        "DESCRIPTION",
        id_width = id_width
    ));

    for playground in playgrounds.values() {
        output.push_str(&format!(
            "{:<id_width$}  {}\n",
            playground.id,
            playground.description,
            id_width = id_width
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use crate::config::{PlaygroundConfig, PlaygroundDefinition};

    use super::format_playgrounds;

    #[test]
    fn formats_playground_table() {
        let mut playgrounds = BTreeMap::new();
        playgrounds.insert(
            "demo".to_string(),
            PlaygroundDefinition {
                id: "demo".to_string(),
                description: "Demo playground".to_string(),
                directory: PathBuf::from("/tmp/demo"),
                config_file: PathBuf::from("/tmp/demo/apg.toml"),
                playground: PlaygroundConfig::default(),
            },
        );
        playgrounds.insert(
            "longer-id".to_string(),
            PlaygroundDefinition {
                id: "longer-id".to_string(),
                description: "Longer playground".to_string(),
                directory: PathBuf::from("/tmp/longer-id"),
                config_file: PathBuf::from("/tmp/longer-id/apg.toml"),
                playground: PlaygroundConfig::default(),
            },
        );

        assert_eq!(
            format_playgrounds(&playgrounds),
            "PLAYGROUND  DESCRIPTION\n\
demo        Demo playground\n\
longer-id   Longer playground\n"
        );
    }

    #[test]
    fn formats_empty_playground_list() {
        assert_eq!(
            format_playgrounds(&BTreeMap::new()),
            "No playgrounds found.\n"
        );
    }
}

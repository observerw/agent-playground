//! Utilities for rendering detailed information about a configured playground.
//!
//! This module powers the CLI `info` subcommand and focuses on
//! presentation-oriented output for a single playground.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result};

use crate::config::{AppConfig, PlaygroundDefinition, ResolvedAgentConfig};

/// Loads application configuration and prints detailed information for one
/// playground.
///
/// # Errors
///
/// Returns an error when configuration loading fails, the playground does not
/// exist, or the playground directory cannot be inspected.
pub fn show_playground_info(playground_id: &str) -> Result<()> {
    let config = AppConfig::load()?;
    let playground = config
        .playgrounds
        .get(playground_id)
        .with_context(|| format!("unknown playground '{playground_id}'"))?;

    print!("{}", format_playground_info(&config, playground)?);
    Ok(())
}

fn format_playground_info(config: &AppConfig, playground: &PlaygroundDefinition) -> Result<String> {
    let effective_config = config.resolve_playground_config(playground)?;
    let default_agent_source = if playground.playground.default_agent.is_some() {
        "playground"
    } else {
        "root"
    };
    let agent_command = config
        .agents
        .get(&effective_config.default_agent)
        .map(|agent| agent.cmd.as_str())
        .with_context(|| {
            format!(
                "default agent '{}' is not defined in [agent.<id>]",
                effective_config.default_agent
            )
        })?;
    let agent_config_dirs = find_agent_config_dirs(&playground.directory, &config.agents)?;

    Ok(format!(
        "PLAYGROUND:         {}\n\
DESCRIPTION:        {}\n\
DIRECTORY:          {}\n\
CONFIG_FILE:        {}\n\
DEFAULT_AGENT:      {} ({})\n\
AGENT_COMMAND:      {}\n\
AGENT_CONFIG_DIRS:  {}\n",
        playground.id,
        playground.description,
        playground.directory.display(),
        playground.config_file.display(),
        effective_config.default_agent,
        default_agent_source,
        agent_command,
        if agent_config_dirs.is_empty() {
            "(none)".to_string()
        } else {
            agent_config_dirs.join(", ")
        }
    ))
}

fn find_agent_config_dirs(
    playground_dir: &Path,
    agents: &BTreeMap<String, ResolvedAgentConfig>,
) -> Result<Vec<String>> {
    let mut agent_config_dirs = BTreeSet::new();

    for agent in agents.values() {
        let path = playground_dir.join(&agent.config_dir);
        if path.is_dir() {
            agent_config_dirs.insert(agent.config_dir.display().to_string());
        }
    }

    Ok(agent_config_dirs.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use tempfile::TempDir;

    use crate::config::{AppConfig, ConfigPaths, PlaygroundDefinition, ResolvedAgentConfig};

    use super::format_playground_info;

    #[test]
    fn formats_playground_info_with_playground_default_agent_and_templates() {
        let temp_dir = TempDir::new().expect("temp dir");
        let playground_dir = temp_dir.path().join("demo");
        fs::create_dir_all(playground_dir.join(".claude")).expect("create claude config dir");
        fs::create_dir_all(playground_dir.join(".codex")).expect("create codex config dir");
        fs::create_dir_all(playground_dir.join(".git")).expect("create git dir");
        let config_file = playground_dir.join("apg.toml");
        fs::write(&config_file, "description = 'Demo playground'").expect("write config");

        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            ResolvedAgentConfig {
                cmd: "claude".to_string(),
                config_dir: ".claude".into(),
            },
        );
        agents.insert(
            "codex".to_string(),
            ResolvedAgentConfig {
                cmd: "codex exec".to_string(),
                config_dir: ".codex".into(),
            },
        );

        let config = AppConfig {
            paths: ConfigPaths::from_root_dir(temp_dir.path().join("config-root")),
            agents,
            default_playground: None,
            saved_playgrounds_dir: temp_dir.path().join("saved-playgrounds"),
            playground_defaults: crate::config::PlaygroundConfig {
                default_agent: Some("claude".to_string()),
                load_env: Some(false),
                create_mode: None,
            },
            playgrounds: BTreeMap::new(),
        };
        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "Demo playground".to_string(),
            directory: playground_dir.clone(),
            config_file,
            playground: crate::config::PlaygroundConfig {
                default_agent: Some("codex".to_string()),
                load_env: None,
                create_mode: None,
            },
        };

        let output = format_playground_info(&config, &playground).expect("format should succeed");

        assert_eq!(
            output,
            format!(
                "PLAYGROUND:         demo\n\
DESCRIPTION:        Demo playground\n\
DIRECTORY:          {}\n\
CONFIG_FILE:        {}\n\
DEFAULT_AGENT:      codex (playground)\n\
AGENT_COMMAND:      codex exec\n\
AGENT_CONFIG_DIRS:  .claude, .codex\n",
                playground_dir.display(),
                playground_dir.join("apg.toml").display()
            )
        );
    }

    #[test]
    fn formats_playground_info_with_inherited_root_default_agent() {
        let temp_dir = TempDir::new().expect("temp dir");
        let playground_dir = temp_dir.path().join("demo");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        let config_file = playground_dir.join("apg.toml");
        fs::write(&config_file, "description = 'Demo playground'").expect("write config");

        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            ResolvedAgentConfig {
                cmd: "claude".to_string(),
                config_dir: ".claude".into(),
            },
        );

        let config = AppConfig {
            paths: ConfigPaths::from_root_dir(temp_dir.path().join("config-root")),
            agents,
            default_playground: None,
            saved_playgrounds_dir: temp_dir.path().join("saved-playgrounds"),
            playground_defaults: crate::config::PlaygroundConfig {
                default_agent: Some("claude".to_string()),
                load_env: Some(false),
                create_mode: None,
            },
            playgrounds: BTreeMap::new(),
        };
        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "Demo playground".to_string(),
            directory: playground_dir.clone(),
            config_file,
            playground: crate::config::PlaygroundConfig::default(),
        };

        let output = format_playground_info(&config, &playground).expect("format should succeed");

        assert_eq!(
            output,
            format!(
                "PLAYGROUND:         demo\n\
DESCRIPTION:        Demo playground\n\
DIRECTORY:          {}\n\
CONFIG_FILE:        {}\n\
DEFAULT_AGENT:      claude (root)\n\
AGENT_COMMAND:      claude\n\
AGENT_CONFIG_DIRS:  (none)\n",
                playground_dir.display(),
                playground_dir.join("apg.toml").display()
            )
        );
    }
}

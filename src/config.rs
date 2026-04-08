//! Configuration models and loaders for `agent-playground`.
//!
//! This module owns three related concerns:
//! - Resolving where configuration files live on disk.
//! - Reading/writing root and per-playground TOML config files.
//! - Producing a fully resolved [`crate::config::AppConfig`] used by runtime
//!   commands.
//!
//! The primary entry points are [`crate::config::AppConfig::load`] and
//! [`crate::config::init_playground`].

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::utils::symlink::copy_symlink;

const APP_CONFIG_DIR: &str = "agent-playground";
const ROOT_CONFIG_FILE_NAME: &str = "config.toml";
const PLAYGROUND_CONFIG_FILE_NAME: &str = "apg.toml";
const PLAYGROUNDS_DIR_NAME: &str = "playgrounds";
const AGENTS_DIR_NAME: &str = "agents";
const DEFAULT_SUBCOMMAND_PLAYGROUND_ID: &str = "default";
const DEFAULT_SAVED_PLAYGROUNDS_DIR_NAME: &str = "saved-playgrounds";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonical filesystem paths used by the application config layer.
pub struct ConfigPaths {
    /// Root directory containing all app-managed config state.
    ///
    /// By default this resolves to `$HOME/.config/agent-playground`.
    pub root_dir: PathBuf,
    /// Path to the root config file (`config.toml`).
    pub config_file: PathBuf,
    /// Directory containing per-playground subdirectories.
    pub playgrounds_dir: PathBuf,
    /// Directory containing per-agent config directories copied during `init`.
    pub agents_dir: PathBuf,
}

impl ConfigPaths {
    /// Builds config paths from the current user's config base directory.
    ///
    /// This resolves to `$HOME/.config/agent-playground` on all platforms.
    pub fn from_user_config_dir() -> Result<Self> {
        let config_dir = user_config_base_dir()?;

        Ok(Self::from_root_dir(config_dir.join(APP_CONFIG_DIR)))
    }

    /// Builds config paths from an explicit root directory.
    pub fn from_root_dir(root_dir: PathBuf) -> Self {
        Self {
            config_file: root_dir.join(ROOT_CONFIG_FILE_NAME),
            playgrounds_dir: root_dir.join(PLAYGROUNDS_DIR_NAME),
            agents_dir: root_dir.join(AGENTS_DIR_NAME),
            root_dir,
        }
    }
}

fn user_config_base_dir() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("failed to locate the user's home directory")?;
    Ok(home_dir.join(".config"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Fully resolved application configuration used by command execution.
///
/// Values in this struct are post-processed defaults/overrides loaded from
/// [`RootConfigFile`] and playground-specific [`PlaygroundConfigFile`] entries.
pub struct AppConfig {
    /// Resolved filesystem locations for all config assets.
    pub paths: ConfigPaths,
    /// Agent identifier to runtime config mapping from `[agent.<id>]`.
    pub agents: BTreeMap<String, ResolvedAgentConfig>,
    /// Optional playground id used when `apg` runs without an explicit id.
    pub default_playground: Option<String>,
    /// Destination directory where saved snapshot copies are written.
    pub saved_playgrounds_dir: PathBuf,
    /// Default playground runtime config inherited by all playgrounds.
    pub playground_defaults: PlaygroundConfig,
    /// All discovered playground definitions keyed by playground id.
    pub playgrounds: BTreeMap<String, PlaygroundDefinition>,
}

impl AppConfig {
    /// Loads and validates application configuration from the default location.
    ///
    /// If the root config does not exist yet, default files/directories are
    /// created first.
    pub fn load() -> Result<Self> {
        Self::load_from_paths(ConfigPaths::from_user_config_dir()?)
    }

    fn load_from_paths(paths: ConfigPaths) -> Result<Self> {
        ensure_root_initialized(&paths)?;
        let resolved_root_config = load_root_config(&paths)?;
        let agents = resolved_root_config.agents;
        let default_playground = resolved_root_config.default_playground;
        let saved_playgrounds_dir = resolve_saved_playgrounds_dir(
            &paths.root_dir,
            resolved_root_config.saved_playgrounds_dir,
        );
        let playground_defaults = resolved_root_config.playground_defaults;

        validate_default_agent_defined(
            &agents,
            playground_defaults.default_agent.as_deref(),
            "default agent",
        )?;

        let playgrounds = load_playgrounds(&paths.playgrounds_dir, &agents, &playground_defaults)?;
        validate_default_playground(&playgrounds, default_playground.as_deref())?;

        Ok(Self {
            paths,
            agents,
            default_playground,
            saved_playgrounds_dir,
            playground_defaults,
            playgrounds,
        })
    }

    /// Returns the effective runtime config for a playground after applying
    /// root-level playground defaults.
    pub(crate) fn resolve_playground_config(
        &self,
        playground: &PlaygroundDefinition,
    ) -> Result<ResolvedPlaygroundConfig> {
        playground
            .playground
            .resolve_over(&self.playground_defaults)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result metadata returned by [`init_playground`].
pub struct InitResult {
    /// The config paths used for initialization.
    pub paths: ConfigPaths,
    /// The initialized playground id.
    pub playground_id: String,
    /// Whether `config.toml` was created as part of this call.
    pub root_config_created: bool,
    /// Whether the playground config file (`apg.toml`) was created.
    pub playground_config_created: bool,
    /// Agent ids whose config directories were initialized in the playground.
    pub initialized_agent_configs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result metadata returned by [`remove_playground`].
pub struct RemoveResult {
    /// The config paths used to resolve the playground location.
    pub paths: ConfigPaths,
    /// The removed playground id.
    pub playground_id: String,
    /// Path to the removed playground directory.
    pub playground_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A resolved playground entry loaded from the `playgrounds/` directory.
pub struct PlaygroundDefinition {
    /// Stable playground identifier (directory name).
    pub id: String,
    /// Human-readable description from `apg.toml`.
    pub description: String,
    /// Path to the playground directory.
    pub directory: PathBuf,
    /// Path to this playground's `apg.toml` file.
    pub config_file: PathBuf,
    /// Per-playground runtime config overrides loaded from `apg.toml`.
    pub playground: PlaygroundConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Lightweight playground metadata for side-effect-free UI surfaces like shell completion.
pub struct ConfiguredPlayground {
    /// Stable playground identifier (directory name).
    pub id: String,
    /// Human-readable description from `apg.toml`.
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
/// Strategy used to materialize playground contents into a temporary directory.
pub enum CreateMode {
    /// Recursively copy files into the temporary directory.
    #[default]
    Copy,
    /// Create symlinks from the temporary directory back to the playground.
    Symlink,
    /// Recreate directories and hard-link regular files into the temporary directory.
    Hardlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
/// Shared playground-scoped config fields used by root defaults and per-playground overrides.
pub struct PlaygroundConfig {
    /// Optional default agent id override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    /// Optional flag controlling `.env` loading in playground runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_env: Option<bool>,
    /// Optional strategy for creating the temporary playground working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_mode: Option<CreateMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
/// Serializable model for one `[agent.<id>]` entry in root `config.toml`.
pub struct AgentConfigFile {
    /// Command used to launch the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Relative destination directory copied during `apg init --agent <id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
/// Serializable model for the root `config.toml` file.
pub struct RootConfigFile {
    /// Agent id to structured config mapping under `[agent.<id>]`.
    #[serde(default)]
    pub agent: BTreeMap<String, AgentConfigFile>,
    /// Optional playground id used when `apg` runs without an explicit id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_playground: Option<String>,
    /// Optional directory for persisted playground snapshots.
    ///
    /// Relative paths are resolved against [`ConfigPaths::root_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_playgrounds_dir: Option<PathBuf>,
    /// Optional defaults inherited by all playgrounds.
    #[serde(default, skip_serializing_if = "PlaygroundConfig::is_empty")]
    pub playground: PlaygroundConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRootConfig {
    agents: BTreeMap<String, ResolvedAgentConfig>,
    default_playground: Option<String>,
    saved_playgrounds_dir: PathBuf,
    playground_defaults: PlaygroundConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Runtime-ready agent configuration resolved from root `config.toml`.
pub struct ResolvedAgentConfig {
    /// Command used to launch the agent.
    pub cmd: String,
    /// Destination directory copied during `apg init --agent <id>`.
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPlaygroundConfig {
    pub(crate) default_agent: String,
    pub(crate) load_env: bool,
    pub(crate) create_mode: CreateMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
/// Serializable model for a playground's `apg.toml` file.
pub struct PlaygroundConfigFile {
    /// Human-readable description shown in listing output.
    pub description: String,
    /// Optional playground-local runtime overrides.
    #[serde(flatten)]
    pub playground: PlaygroundConfig,
}

impl PlaygroundConfig {
    fn builtin_defaults() -> Self {
        Self {
            default_agent: Some("claude".to_string()),
            load_env: Some(false),
            create_mode: Some(CreateMode::Copy),
        }
    }

    fn is_empty(&self) -> bool {
        self.default_agent.is_none() && self.load_env.is_none() && self.create_mode.is_none()
    }

    fn merged_over(&self, base: &Self) -> Self {
        Self {
            default_agent: self
                .default_agent
                .clone()
                .or_else(|| base.default_agent.clone()),
            load_env: self.load_env.or(base.load_env),
            create_mode: self.create_mode.or(base.create_mode),
        }
    }

    fn resolve_over(&self, base: &Self) -> Result<ResolvedPlaygroundConfig> {
        let merged = self.merged_over(base);

        Ok(ResolvedPlaygroundConfig {
            default_agent: merged
                .default_agent
                .context("default playground config is missing default_agent")?,
            load_env: merged.load_env.unwrap_or(false),
            create_mode: merged.create_mode.unwrap_or(CreateMode::Copy),
        })
    }
}

impl AgentConfigFile {
    fn merged_over(&self, base: &Self) -> Self {
        Self {
            cmd: self.cmd.clone().or_else(|| base.cmd.clone()),
            config_dir: self.config_dir.clone().or_else(|| base.config_dir.clone()),
        }
    }

    fn resolve(&self, agent_id: &str) -> Result<ResolvedAgentConfig> {
        let cmd = self.cmd.clone().unwrap_or_else(|| agent_id.to_string());
        let config_dir = self
            .config_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!(".{agent_id}/")));
        let config_dir = normalize_agent_config_dir(agent_id, &config_dir)?;

        Ok(ResolvedAgentConfig { cmd, config_dir })
    }
}

impl RootConfigFile {
    /// Returns a JSON Schema for the root config file format.
    pub fn json_schema() -> Schema {
        schema_for!(Self)
    }

    fn defaults_for_paths(paths: &ConfigPaths) -> Self {
        let mut agent = BTreeMap::new();
        agent.insert(
            "claude".to_string(),
            AgentConfigFile {
                cmd: Some("claude".to_string()),
                config_dir: Some(PathBuf::from(".claude/")),
            },
        );
        agent.insert(
            "opencode".to_string(),
            AgentConfigFile {
                cmd: Some("opencode".to_string()),
                config_dir: Some(PathBuf::from(".opencode/")),
            },
        );

        Self {
            agent,
            default_playground: None,
            saved_playgrounds_dir: Some(default_saved_playgrounds_dir(paths)),
            playground: PlaygroundConfig::builtin_defaults(),
        }
    }

    fn resolve(self, paths: &ConfigPaths) -> Result<ResolvedRootConfig> {
        let defaults = Self::defaults_for_paths(paths);
        let mut merged_agents = defaults.agent;
        for (agent_id, agent_config) in self.agent {
            if let Some(default_agent_config) = merged_agents.get(&agent_id) {
                merged_agents.insert(agent_id, agent_config.merged_over(default_agent_config));
            } else {
                merged_agents.insert(agent_id, agent_config);
            }
        }
        let mut agents = BTreeMap::new();
        for (agent_id, agent_config) in merged_agents {
            validate_agent_id(&agent_id)
                .with_context(|| format!("invalid agent id in root config: '{agent_id}'"))?;
            agents.insert(agent_id.clone(), agent_config.resolve(&agent_id)?);
        }
        let default_playground = self.default_playground;

        let saved_playgrounds_dir = self
            .saved_playgrounds_dir
            .or(defaults.saved_playgrounds_dir)
            .context("default root config is missing saved_playgrounds_dir")?;
        let playground_defaults = self.playground.merged_over(&defaults.playground);

        Ok(ResolvedRootConfig {
            agents,
            default_playground,
            saved_playgrounds_dir,
            playground_defaults,
        })
    }
}

impl PlaygroundConfigFile {
    /// Returns a JSON Schema for the playground config file format.
    pub fn json_schema() -> Schema {
        schema_for!(Self)
    }

    fn for_playground(playground_id: &str) -> Self {
        Self {
            description: format!("TODO: describe {playground_id}"),
            playground: PlaygroundConfig::default(),
        }
    }
}

/// Initializes a new playground directory and config file.
///
/// The playground is created under `playgrounds/<playground_id>`.
/// When `agent_ids` are provided, matching configured agent directories under
/// `agents/<agent_id>/` are copied into the configured `config_dir`.
pub fn init_playground(playground_id: &str, agent_ids: &[String]) -> Result<InitResult> {
    init_playground_at(
        ConfigPaths::from_user_config_dir()?,
        playground_id,
        agent_ids,
    )
}

fn init_playground_at(
    paths: ConfigPaths,
    playground_id: &str,
    agent_ids: &[String],
) -> Result<InitResult> {
    init_playground_at_with_git(
        paths,
        playground_id,
        agent_ids,
        git_is_available,
        init_git_repo,
    )
}

fn init_playground_at_with_git<GA, GI>(
    paths: ConfigPaths,
    playground_id: &str,
    agent_ids: &[String],
    git_is_available: GA,
    init_git_repo: GI,
) -> Result<InitResult>
where
    GA: Fn() -> Result<bool>,
    GI: Fn(&Path) -> Result<()>,
{
    validate_playground_id(playground_id)?;
    let root_config_created = ensure_root_initialized(&paths)?;
    let root_config = load_root_config(&paths)?;
    let selected_agent_configs = select_agent_configs(&paths, &root_config.agents, agent_ids)?;

    let playground_dir = paths.playgrounds_dir.join(playground_id);
    let playground_config_file = playground_dir.join(PLAYGROUND_CONFIG_FILE_NAME);

    if playground_config_file.exists() {
        bail!(
            "playground '{}' already exists at {}",
            playground_id,
            playground_config_file.display()
        );
    }

    fs::create_dir_all(&playground_dir)
        .with_context(|| format!("failed to create {}", playground_dir.display()))?;
    write_toml_file(
        &playground_config_file,
        &PlaygroundConfigFile::for_playground(playground_id),
    )?;
    copy_agent_configs(&playground_dir, &selected_agent_configs)?;
    if git_is_available()?
        && let Err(error) = init_git_repo(&playground_dir)
    {
        match fs::remove_dir_all(&playground_dir) {
            Ok(()) => {
                return Err(error).context(format!(
                    "failed to initialize git repository in {}; removed partially initialized playground",
                    playground_dir.display()
                ));
            }
            Err(cleanup_error) => {
                return Err(error).context(format!(
                    "failed to initialize git repository in {}; additionally failed to remove partially initialized playground {}: {cleanup_error}",
                    playground_dir.display(),
                    playground_dir.display()
                ));
            }
        }
    }

    Ok(InitResult {
        paths,
        playground_id: playground_id.to_string(),
        root_config_created,
        playground_config_created: true,
        initialized_agent_configs: selected_agent_configs
            .iter()
            .map(|agent| agent.agent_id.clone())
            .collect(),
    })
}

/// Returns the ids of configured playgrounds without mutating user config.
///
/// Unlike [`AppConfig::load`], this does not create default config files or
/// directories when they are missing. Invalid or incomplete playground
/// directories are ignored so completion-oriented callers can fail soft.
pub fn configured_playground_ids() -> Result<Vec<String>> {
    Ok(configured_playgrounds()?
        .into_iter()
        .map(|playground| playground.id)
        .collect())
}

/// Returns configured playground metadata without mutating user config.
///
/// Unlike [`AppConfig::load`], this does not create default config files or
/// directories when they are missing. Invalid or incomplete playground
/// directories are ignored so completion-oriented callers can fail soft.
pub fn configured_playgrounds() -> Result<Vec<ConfiguredPlayground>> {
    configured_playgrounds_at(&ConfigPaths::from_user_config_dir()?.playgrounds_dir)
}

/// Resolves an existing playground directory under the global config root.
pub fn resolve_playground_dir(playground_id: &str) -> Result<PathBuf> {
    resolve_playground_dir_at(ConfigPaths::from_user_config_dir()?, playground_id)
}

/// Removes a playground directory from the global config root.
pub fn remove_playground(playground_id: &str) -> Result<RemoveResult> {
    let paths = ConfigPaths::from_user_config_dir()?;
    remove_playground_at(paths, playground_id)
}

fn remove_playground_at(paths: ConfigPaths, playground_id: &str) -> Result<RemoveResult> {
    let playground_dir = resolve_playground_dir_at(paths.clone(), playground_id)?;

    fs::remove_dir_all(&playground_dir)
        .with_context(|| format!("failed to remove {}", playground_dir.display()))?;

    Ok(RemoveResult {
        paths,
        playground_id: playground_id.to_string(),
        playground_dir,
    })
}

fn resolve_playground_dir_at(paths: ConfigPaths, playground_id: &str) -> Result<PathBuf> {
    validate_playground_id(playground_id)?;

    let playground_dir = paths.playgrounds_dir.join(playground_id);
    if !playground_dir.exists() {
        bail!("unknown playground '{playground_id}'");
    }

    let metadata = fs::symlink_metadata(&playground_dir)
        .with_context(|| format!("failed to inspect {}", playground_dir.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "playground '{}' cannot be removed because it is a symlink: {}",
            playground_id,
            playground_dir.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "playground '{}' is not a directory: {}",
            playground_id,
            playground_dir.display()
        );
    }

    Ok(playground_dir)
}

fn configured_playgrounds_at(playgrounds_dir: &Path) -> Result<Vec<ConfiguredPlayground>> {
    if !playgrounds_dir.exists() {
        return Ok(Vec::new());
    }

    if !playgrounds_dir.is_dir() {
        bail!(
            "playground config path is not a directory: {}",
            playgrounds_dir.display()
        );
    }

    let mut playgrounds = Vec::new();
    for entry_result in fs::read_dir(playgrounds_dir)
        .with_context(|| format!("failed to read {}", playgrounds_dir.display()))?
    {
        let Ok(entry) = entry_result else {
            // Skip entries that cannot be inspected (e.g., PermissionDenied).
            continue;
        };

        let Ok(file_type) = entry.file_type() else {
            // Skip entries whose type cannot be determined.
            continue;
        };

        if !file_type.is_dir() {
            continue;
        }

        let playground_id = entry.file_name().to_string_lossy().into_owned();
        if validate_playground_id(&playground_id).is_err() {
            continue;
        }

        let config_file = entry.path().join(PLAYGROUND_CONFIG_FILE_NAME);
        if !config_file.is_file() {
            continue;
        }

        let Ok(playground_config) = read_toml_file::<PlaygroundConfigFile>(&config_file) else {
            continue;
        };

        playgrounds.push(ConfiguredPlayground {
            id: playground_id,
            description: playground_config.description,
        });
    }

    playgrounds.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(playgrounds)
}

fn validate_playground_id(playground_id: &str) -> Result<()> {
    if playground_id.is_empty() {
        bail!("playground id cannot be empty");
    }
    if playground_id == DEFAULT_SUBCOMMAND_PLAYGROUND_ID {
        bail!(
            "invalid playground id '{playground_id}': this name is reserved for the `default` subcommand"
        );
    }
    if playground_id.starts_with("__") {
        bail!(
            "invalid playground id '{playground_id}': ids starting with '__' are reserved for internal use"
        );
    }
    if matches!(playground_id, "." | "..")
        || playground_id.contains('/')
        || playground_id.contains('\\')
    {
        bail!(
            "invalid playground id '{}': ids must not contain path separators or parent-directory segments",
            playground_id
        );
    }

    Ok(())
}

fn validate_agent_id(agent_id: &str) -> Result<()> {
    if agent_id.is_empty() {
        bail!("agent id cannot be empty");
    }
    if matches!(agent_id, "." | "..") || agent_id.contains('/') || agent_id.contains('\\') {
        bail!(
            "invalid agent id '{}': ids must not contain path separators or parent-directory segments",
            agent_id
        );
    }

    Ok(())
}

fn git_is_available() -> Result<bool> {
    match Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => Ok(status.success()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to check whether git is available"),
    }
}

fn init_git_repo(playground_dir: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("init")
        .current_dir(playground_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "failed to initialize git repository in {}",
                playground_dir.display()
            )
        })?;

    if !status.success() {
        bail!(
            "git init exited with status {status} in {}",
            playground_dir.display()
        );
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedAgentConfig {
    agent_id: String,
    source_dir: PathBuf,
    destination_dir: PathBuf,
}

fn select_agent_configs(
    paths: &ConfigPaths,
    agents: &BTreeMap<String, ResolvedAgentConfig>,
    agent_ids: &[String],
) -> Result<Vec<SelectedAgentConfig>> {
    let available_agent_ids = agents.keys().cloned().collect::<Vec<_>>();
    let mut selected_agents = Vec::new();
    let mut destination_agents: BTreeMap<PathBuf, String> = BTreeMap::new();

    for agent_id in agent_ids {
        validate_agent_id(agent_id)?;

        if selected_agents
            .iter()
            .any(|selected_agent: &SelectedAgentConfig| &selected_agent.agent_id == agent_id)
        {
            continue;
        }

        let agent_config = agents.get(agent_id).with_context(|| {
            format!(
                "unknown agent '{agent_id}'. Available agents: {}",
                if available_agent_ids.is_empty() {
                    "(none)".to_string()
                } else {
                    available_agent_ids.join(", ")
                }
            )
        })?;
        if let Some(existing_agent_id) = destination_agents.get(&agent_config.config_dir) {
            bail!(
                "agent config_dir conflict: '{agent_id}' and '{existing_agent_id}' both target '{}'",
                agent_config.config_dir.display()
            );
        }

        destination_agents.insert(agent_config.config_dir.clone(), agent_id.clone());
        selected_agents.push(SelectedAgentConfig {
            agent_id: agent_id.clone(),
            source_dir: paths.agents_dir.join(agent_id),
            destination_dir: agent_config.config_dir.clone(),
        });
    }

    Ok(selected_agents)
}

fn copy_agent_configs(playground_dir: &Path, agent_configs: &[SelectedAgentConfig]) -> Result<()> {
    for agent_config in agent_configs {
        let destination = playground_dir.join(&agent_config.destination_dir);
        fs::create_dir_all(&destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;

        if !agent_config.source_dir.exists() {
            continue;
        }

        let source_metadata =
            fs::symlink_metadata(&agent_config.source_dir).with_context(|| {
                format!(
                    "failed to inspect {} for agent '{}'",
                    agent_config.source_dir.display(),
                    agent_config.agent_id
                )
            })?;
        if !source_metadata.is_dir() {
            bail!(
                "agent config source for '{}' must be a directory: {}",
                agent_config.agent_id,
                agent_config.source_dir.display()
            );
        }

        copy_directory_contents_recursively(&agent_config.source_dir, &destination)?;
    }

    Ok(())
}

fn copy_directory_contents_recursively(source_dir: &Path, destination_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(source_dir)
        .with_context(|| format!("failed to read {}", source_dir.display()))?
    {
        let entry = entry.with_context(|| {
            format!("failed to inspect an entry under {}", source_dir.display())
        })?;
        let source_path = entry.path();
        let destination_path = destination_dir.join(entry.file_name());
        let file_type = entry.file_type().with_context(|| {
            format!("failed to inspect file type for {}", source_path.display())
        })?;

        if file_type.is_dir() {
            fs::create_dir_all(&destination_path)
                .with_context(|| format!("failed to create {}", destination_path.display()))?;
            copy_directory_contents_recursively(&source_path, &destination_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else {
            bail!(
                "unsupported entry in agent config source: {}",
                source_path.display()
            );
        }
    }

    Ok(())
}

fn ensure_root_initialized(paths: &ConfigPaths) -> Result<bool> {
    fs::create_dir_all(&paths.root_dir)
        .with_context(|| format!("failed to create {}", paths.root_dir.display()))?;
    fs::create_dir_all(&paths.playgrounds_dir)
        .with_context(|| format!("failed to create {}", paths.playgrounds_dir.display()))?;
    fs::create_dir_all(&paths.agents_dir)
        .with_context(|| format!("failed to create {}", paths.agents_dir.display()))?;

    if paths.config_file.exists() {
        return Ok(false);
    }

    write_toml_file(
        &paths.config_file,
        &RootConfigFile::defaults_for_paths(paths),
    )?;

    Ok(true)
}

fn load_root_config(paths: &ConfigPaths) -> Result<ResolvedRootConfig> {
    read_toml_file::<RootConfigFile>(&paths.config_file)?.resolve(paths)
}

fn default_saved_playgrounds_dir(_paths: &ConfigPaths) -> PathBuf {
    PathBuf::from(DEFAULT_SAVED_PLAYGROUNDS_DIR_NAME)
}

fn resolve_saved_playgrounds_dir(root_dir: &Path, configured_path: PathBuf) -> PathBuf {
    if configured_path.is_absolute() {
        return configured_path;
    }

    root_dir.join(configured_path)
}

fn normalize_agent_config_dir(agent_id: &str, config_dir: &Path) -> Result<PathBuf> {
    if config_dir.as_os_str().is_empty() {
        bail!("agent '{agent_id}' config_dir cannot be empty");
    }

    let mut normalized = PathBuf::new();
    for component in config_dir.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                bail!("agent '{agent_id}' config_dir must not contain '..'");
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("agent '{agent_id}' config_dir must be a relative path");
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        bail!("agent '{agent_id}' config_dir cannot be empty");
    }

    Ok(normalized)
}

fn validate_default_agent_defined(
    agents: &BTreeMap<String, ResolvedAgentConfig>,
    default_agent: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(default_agent) = default_agent else {
        bail!("{label} is missing");
    };

    if !agents.contains_key(default_agent) {
        bail!("{label} '{default_agent}' is not defined in [agent.<id>]");
    }

    Ok(())
}

fn validate_default_playground(
    playgrounds: &BTreeMap<String, PlaygroundDefinition>,
    default_playground: Option<&str>,
) -> Result<()> {
    let Some(default_playground) = default_playground else {
        return Ok(());
    };

    validate_playground_id(default_playground)
        .with_context(|| "default_playground is invalid".to_string())?;

    if !playgrounds.contains_key(default_playground) {
        bail!("default_playground '{default_playground}' is not a configured playground");
    }

    Ok(())
}

fn load_playgrounds(
    playgrounds_dir: &Path,
    agents: &BTreeMap<String, ResolvedAgentConfig>,
    playground_defaults: &PlaygroundConfig,
) -> Result<BTreeMap<String, PlaygroundDefinition>> {
    if !playgrounds_dir.exists() {
        return Ok(BTreeMap::new());
    }

    if !playgrounds_dir.is_dir() {
        bail!(
            "playground config path is not a directory: {}",
            playgrounds_dir.display()
        );
    }

    let mut playgrounds = BTreeMap::new();

    for entry in fs::read_dir(playgrounds_dir)
        .with_context(|| format!("failed to read {}", playgrounds_dir.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect an entry under {}",
                playgrounds_dir.display()
            )
        })?;
        let file_type = entry.file_type().with_context(|| {
            format!("failed to inspect file type for {}", entry.path().display())
        })?;

        if !file_type.is_dir() {
            continue;
        }

        let directory = entry.path();
        let config_file = directory.join(PLAYGROUND_CONFIG_FILE_NAME);

        if !config_file.is_file() {
            bail!(
                "playground '{}' is missing {}",
                directory.file_name().unwrap_or_default().to_string_lossy(),
                PLAYGROUND_CONFIG_FILE_NAME
            );
        }

        let playground_config: PlaygroundConfigFile = read_toml_file(&config_file)?;
        let id = entry.file_name().to_string_lossy().into_owned();
        validate_playground_id(&id).with_context(|| {
            format!(
                "invalid playground directory under {}",
                playgrounds_dir.display()
            )
        })?;
        let effective_config = playground_config
            .playground
            .merged_over(playground_defaults);
        validate_default_agent_defined(
            agents,
            effective_config.default_agent.as_deref(),
            &format!("playground '{id}' default agent"),
        )?;

        playgrounds.insert(
            id.clone(),
            PlaygroundDefinition {
                id,
                description: playground_config.description,
                directory,
                config_file,
                playground: playground_config.playground,
            },
        );
    }

    Ok(playgrounds)
}

fn read_toml_file<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    toml::from_str(&content)
        .with_context(|| format!("failed to parse TOML from {}", path.display()))
}

fn write_toml_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let content =
        toml::to_string_pretty(value).context("failed to serialize configuration to TOML")?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{
        APP_CONFIG_DIR, AppConfig, ConfigPaths, ConfiguredPlayground, CreateMode,
        PlaygroundConfigFile, RootConfigFile, configured_playgrounds_at, init_playground_at,
        init_playground_at_with_git, read_toml_file, remove_playground_at,
        resolve_playground_dir_at, user_config_base_dir,
    };
    use serde_json::Value;
    use std::{cell::Cell, fs, io};
    use tempfile::TempDir;

    #[cfg(unix)]
    fn create_test_symlink(source: &std::path::Path, destination: &std::path::Path) {
        std::os::unix::fs::symlink(source, destination).expect("create symlink");
    }

    #[cfg(windows)]
    fn create_test_symlink(source: &std::path::Path, destination: &std::path::Path) {
        std::os::windows::fs::symlink_file(source, destination).expect("create symlink");
    }

    fn resolved_agent_cmd(config: &AppConfig, agent_id: &str) -> Option<String> {
        config.agents.get(agent_id).map(|agent| agent.cmd.clone())
    }

    fn resolved_agent_config_dir(config: &AppConfig, agent_id: &str) -> Option<std::path::PathBuf> {
        config
            .agents
            .get(agent_id)
            .map(|agent| agent.config_dir.clone())
    }

    #[test]
    fn init_creates_root_and_playground_configs_from_file_models() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let result = init_playground_at(paths.clone(), "demo", &[]).expect("init should succeed");

        assert!(result.root_config_created);
        assert!(result.playground_config_created);
        assert!(result.initialized_agent_configs.is_empty());
        assert!(temp_dir.path().join("config.toml").is_file());
        assert!(
            temp_dir
                .path()
                .join("playgrounds")
                .join("demo")
                .join("apg.toml")
                .is_file()
        );
        assert!(
            !temp_dir
                .path()
                .join("playgrounds")
                .join("demo")
                .join(".claude")
                .exists()
        );
        assert_eq!(
            read_toml_file::<RootConfigFile>(&temp_dir.path().join("config.toml"))
                .expect("root config"),
            RootConfigFile::defaults_for_paths(&paths)
        );
        assert_eq!(
            read_toml_file::<PlaygroundConfigFile>(
                &temp_dir
                    .path()
                    .join("playgrounds")
                    .join("demo")
                    .join("apg.toml")
            )
            .expect("playground config"),
            PlaygroundConfigFile::for_playground("demo")
        );

        let config = AppConfig::load_from_paths(paths).expect("config should load");
        assert_eq!(
            resolved_agent_cmd(&config, "claude"),
            Some("claude".to_string())
        );
        assert_eq!(
            resolved_agent_cmd(&config, "opencode"),
            Some("opencode".to_string())
        );
        assert_eq!(
            resolved_agent_config_dir(&config, "claude"),
            Some(std::path::PathBuf::from(".claude"))
        );
        assert_eq!(
            resolved_agent_config_dir(&config, "opencode"),
            Some(std::path::PathBuf::from(".opencode"))
        );
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("claude")
        );
        assert_eq!(config.default_playground, None);
        assert_eq!(config.playground_defaults.load_env, Some(false));
        assert_eq!(
            config.playground_defaults.create_mode,
            Some(CreateMode::Copy)
        );
        assert_eq!(
            config.saved_playgrounds_dir,
            temp_dir.path().join("saved-playgrounds")
        );
        assert_eq!(
            config
                .playgrounds
                .get("demo")
                .expect("demo playground")
                .description,
            "TODO: describe demo"
        );
        assert!(
            config
                .playgrounds
                .get("demo")
                .expect("demo playground")
                .playground
                .is_empty()
        );
    }

    #[test]
    fn merges_root_agents_and_loads_playgrounds() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();
        fs::write(
            root.join("config.toml"),
            r#"saved_playgrounds_dir = "archives"
default_playground = "demo"

[agent.claude]
cmd = "custom-claude"

[agent.codex]
cmd = "codex --fast"

[playground]
default_agent = "codex"
load_env = true
create_mode = "hardlink"
"#,
        )
        .expect("write root config");

        let playground_dir = root.join("playgrounds").join("demo");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(
            playground_dir.join("apg.toml"),
            r#"description = "Demo playground"
default_agent = "claude""#,
        )
        .expect("write playground config");

        let config = AppConfig::load_from_paths(ConfigPaths::from_root_dir(root.to_path_buf()))
            .expect("config should load");

        assert_eq!(
            resolved_agent_cmd(&config, "claude"),
            Some("custom-claude".to_string())
        );
        assert_eq!(
            resolved_agent_cmd(&config, "opencode"),
            Some("opencode".to_string())
        );
        assert_eq!(
            resolved_agent_cmd(&config, "codex"),
            Some("codex --fast".to_string())
        );
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("codex")
        );
        assert_eq!(config.default_playground.as_deref(), Some("demo"));
        assert_eq!(config.playground_defaults.load_env, Some(true));
        assert_eq!(
            config.playground_defaults.create_mode,
            Some(CreateMode::Hardlink)
        );
        assert_eq!(config.saved_playgrounds_dir, root.join("archives"));

        let playground = config.playgrounds.get("demo").expect("demo playground");
        assert_eq!(playground.description, "Demo playground");
        assert_eq!(
            playground.playground.default_agent.as_deref(),
            Some("claude")
        );
        assert_eq!(playground.directory, playground_dir);
        let effective_config = config
            .resolve_playground_config(playground)
            .expect("effective playground config");
        assert_eq!(effective_config.default_agent, "claude");
        assert!(effective_config.load_env);
        assert_eq!(effective_config.create_mode, CreateMode::Hardlink);
    }

    #[test]
    fn playground_create_mode_overrides_root_default() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[playground]
create_mode = "copy"
"#,
        )
        .expect("write root config");
        let playground_dir = temp_dir.path().join("playgrounds").join("demo");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(
            playground_dir.join("apg.toml"),
            r#"description = "Demo playground"
create_mode = "symlink""#,
        )
        .expect("write playground config");

        let config =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect("config should load");
        let playground = config.playgrounds.get("demo").expect("demo playground");
        let effective_config = config
            .resolve_playground_config(playground)
            .expect("effective playground config");

        assert_eq!(
            config.playground_defaults.create_mode,
            Some(CreateMode::Copy)
        );
        assert_eq!(playground.playground.create_mode, Some(CreateMode::Symlink));
        assert_eq!(effective_config.create_mode, CreateMode::Symlink);
    }

    #[test]
    fn errors_when_playground_default_agent_is_not_defined() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");
        let playground_dir = temp_dir.path().join("playgrounds").join("demo");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(
            playground_dir.join("apg.toml"),
            r#"description = "Demo playground"
default_agent = "codex""#,
        )
        .expect("write playground config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("undefined playground default agent should fail");

        assert!(
            error
                .to_string()
                .contains("playground 'demo' default agent 'codex' is not defined")
        );
    }

    #[test]
    fn load_auto_initializes_missing_root_config() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let config = AppConfig::load_from_paths(paths).expect("missing root config should init");

        assert!(temp_dir.path().join("config.toml").is_file());
        assert!(temp_dir.path().join("playgrounds").is_dir());
        assert!(temp_dir.path().join("agents").is_dir());
        assert_eq!(
            resolved_agent_cmd(&config, "claude"),
            Some("claude".to_string())
        );
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("claude")
        );
        assert_eq!(config.default_playground, None);
        assert_eq!(config.playground_defaults.load_env, Some(false));
        assert_eq!(
            config.playground_defaults.create_mode,
            Some(CreateMode::Copy)
        );
        assert_eq!(
            config.saved_playgrounds_dir,
            temp_dir.path().join("saved-playgrounds")
        );
    }

    #[test]
    fn respects_absolute_saved_playgrounds_dir() {
        let temp_dir = TempDir::new().expect("temp dir");
        let archive_dir = TempDir::new().expect("archive dir");
        let archive_path = archive_dir
            .path()
            .display()
            .to_string()
            .replace('\\', "\\\\");
        fs::write(
            temp_dir.path().join("config.toml"),
            format!(
                r#"saved_playgrounds_dir = "{}"

[agent.claude]
cmd = "claude"
"#,
                archive_path
            ),
        )
        .expect("write root config");

        let config =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect("config should load");

        assert_eq!(config.saved_playgrounds_dir, archive_dir.path());
    }

    #[test]
    fn errors_when_playground_config_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.claude]
cmd = "claude"

[agent.opencode]
cmd = "opencode"
"#,
        )
        .expect("write root config");
        let playground_dir = temp_dir.path().join("playgrounds").join("broken");
        fs::create_dir_all(&playground_dir).expect("create playground dir");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("missing playground config should fail");

        assert!(error.to_string().contains("missing apg.toml"));
    }

    #[test]
    fn errors_when_default_agent_is_not_defined() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[playground]
default_agent = "codex""#,
        )
        .expect("write root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("undefined default agent should fail");

        assert!(
            error
                .to_string()
                .contains("default agent 'codex' is not defined")
        );
    }

    #[test]
    fn errors_when_default_playground_is_not_configured() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"default_playground = "missing"

[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("unknown default playground should fail");

        assert!(
            error
                .to_string()
                .contains("default_playground 'missing' is not a configured playground")
        );
    }

    #[test]
    fn errors_when_default_playground_uses_reserved_name() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"default_playground = "default"

[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("reserved default playground should fail");
        let message = format!("{error:#}");

        assert!(message.contains("default_playground is invalid"));
        assert!(message.contains("reserved for the `default` subcommand"));
    }

    #[test]
    fn init_errors_when_playground_already_exists() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        init_playground_at(paths.clone(), "demo", &[]).expect("initial init should succeed");
        let error = init_playground_at(paths, "demo", &[]).expect_err("duplicate init should fail");

        assert!(
            error
                .to_string()
                .contains("playground 'demo' already exists")
        );
    }

    #[test]
    fn init_rejects_reserved_default_playground_id() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error = init_playground_at(paths, "default", &[]).expect_err("reserved id should fail");

        assert!(
            error
                .to_string()
                .contains("invalid playground id 'default'")
        );
        assert!(
            error
                .to_string()
                .contains("reserved for the `default` subcommand")
        );
    }

    #[test]
    fn init_rejects_internal_reserved_playground_id_prefix() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error =
            init_playground_at(paths, "__default__", &[]).expect_err("reserved id should fail");

        assert!(
            error
                .to_string()
                .contains("ids starting with '__' are reserved for internal use")
        );
    }

    #[test]
    fn remove_deletes_existing_playground_directory() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let nested_file = temp_dir
            .path()
            .join("playgrounds")
            .join("demo")
            .join("notes.txt");

        init_playground_at(paths.clone(), "demo", &[]).expect("init should succeed");
        fs::write(&nested_file, "hello").expect("write nested file");

        let result = remove_playground_at(paths.clone(), "demo").expect("remove should succeed");

        assert_eq!(result.paths, paths);
        assert_eq!(result.playground_id, "demo");
        assert_eq!(
            result.playground_dir,
            temp_dir.path().join("playgrounds").join("demo")
        );
        assert!(!result.playground_dir.exists());
    }

    #[test]
    fn remove_errors_for_unknown_playground() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error =
            remove_playground_at(paths, "missing").expect_err("missing playground should fail");

        assert!(error.to_string().contains("unknown playground 'missing'"));
    }

    #[test]
    fn resolve_playground_dir_rejects_path_traversal_ids() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error = resolve_playground_dir_at(paths, "../demo")
            .expect_err("path traversal playground id should fail");

        assert!(
            error
                .to_string()
                .contains("invalid playground id '../demo'")
        );
    }

    #[test]
    fn init_rejects_path_traversal_ids_before_writing_files() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error = init_playground_at(paths, "../demo", &[])
            .expect_err("path traversal playground id should fail");

        assert!(
            error
                .to_string()
                .contains("invalid playground id '../demo'")
        );
        assert!(!temp_dir.path().join("config.toml").exists());
        assert!(!temp_dir.path().join("playgrounds").exists());
        assert!(!temp_dir.path().join("playgrounds").join("demo").exists());
    }

    #[test]
    fn init_cleans_up_playground_directory_when_git_init_fails() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let error = init_playground_at_with_git(
            paths,
            "demo",
            &[],
            || Ok(true),
            |_| Err(io::Error::other("git init failed").into()),
        )
        .expect_err("git init failure should fail init");

        let error_message = format!("{error:#}");

        assert!(error_message.contains("git init failed"));
        assert!(error_message.contains("removed partially initialized playground"));
        assert!(!temp_dir.path().join("playgrounds").join("demo").exists());
    }

    #[test]
    fn init_copies_existing_agent_sources_and_creates_missing_targets() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec!["claude".to_string(), "opencode".to_string()];

        let claude_source_dir = paths.agents_dir.join("claude");
        fs::create_dir_all(&claude_source_dir).expect("create claude source");
        fs::write(
            claude_source_dir.join("settings.json"),
            r#"{"theme":"dark"}"#,
        )
        .expect("write claude source file");

        let result =
            init_playground_at(paths, "demo", &selected_agents).expect("init should succeed");
        let playground_dir = temp_dir.path().join("playgrounds").join("demo");

        assert_eq!(
            result.initialized_agent_configs,
            vec!["claude".to_string(), "opencode".to_string()]
        );
        assert!(
            playground_dir
                .join(".claude")
                .join("settings.json")
                .is_file()
        );
        assert!(playground_dir.join(".opencode").is_dir());
    }

    #[test]
    fn init_initializes_git_repo_when_git_is_available() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let git_init_called = Cell::new(false);

        init_playground_at_with_git(
            paths,
            "demo",
            &[],
            || Ok(true),
            |playground_dir| {
                git_init_called.set(true);
                fs::create_dir(playground_dir.join(".git")).expect("create .git directory");
                Ok(())
            },
        )
        .expect("init should succeed");

        assert!(git_init_called.get());
        assert!(
            temp_dir
                .path()
                .join("playgrounds")
                .join("demo")
                .join(".git")
                .is_dir()
        );
    }

    #[test]
    fn init_skips_git_repo_when_git_is_unavailable() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let git_init_called = Cell::new(false);

        init_playground_at_with_git(
            paths,
            "demo",
            &[],
            || Ok(false),
            |_| {
                git_init_called.set(true);
                Ok(())
            },
        )
        .expect("init should succeed");

        assert!(!git_init_called.get());
        assert!(
            !temp_dir
                .path()
                .join("playgrounds")
                .join("demo")
                .join(".git")
                .exists()
        );
    }

    #[test]
    fn init_deduplicates_selected_agent_configs() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec![
            "claude".to_string(),
            "claude".to_string(),
            "opencode".to_string(),
        ];

        let result =
            init_playground_at(paths, "demo", &selected_agents).expect("init should succeed");

        assert_eq!(
            result.initialized_agent_configs,
            vec!["claude".to_string(), "opencode".to_string()]
        );
    }

    #[test]
    fn init_errors_for_unknown_agent_before_creating_playground() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec!["missing".to_string()];

        let error = init_playground_at(paths, "demo", &selected_agents)
            .expect_err("unknown agent should fail");

        assert!(error.to_string().contains("unknown agent 'missing'"));
        assert!(!temp_dir.path().join("playgrounds").join("demo").exists());
    }

    #[test]
    fn init_errors_when_selected_agents_share_the_same_config_dir() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root_dir = temp_dir.path();
        fs::write(
            root_dir.join("config.toml"),
            r#"[agent.alpha]
cmd = "alpha"
config_dir = ".shared/"

[agent.beta]
cmd = "beta"
config_dir = ".shared/"
"#,
        )
        .expect("write root config");

        let error = init_playground_at(
            ConfigPaths::from_root_dir(root_dir.to_path_buf()),
            "demo",
            &["alpha".to_string(), "beta".to_string()],
        )
        .expect_err("conflicting config_dir should fail");

        assert!(error.to_string().contains("agent config_dir conflict"));
    }

    #[test]
    fn errors_when_agent_config_dir_is_not_safe_relative_path() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.bad]
cmd = "bad"
config_dir = "../outside"
"#,
        )
        .expect("write root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("unsafe config_dir should fail");

        assert!(error.to_string().contains("config_dir"));
        assert!(error.to_string().contains("must not contain '..'"));
    }

    #[test]
    fn errors_when_agent_id_is_not_safe_relative_key() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent."../escape"]
cmd = "bad"
"#,
        )
        .expect("write root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("invalid agent id should fail");

        assert!(error.to_string().contains("invalid agent id"));
    }

    #[test]
    fn init_copies_symlinks_from_agent_source_directory() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let source_dir = paths.agents_dir.join("claude");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::write(source_dir.join("settings.json"), "{}").expect("write source file");
        create_test_symlink(
            std::path::Path::new("settings.json"),
            &source_dir.join("settings.link"),
        );

        init_playground_at(paths, "demo", &["claude".to_string()]).expect("init should succeed");

        let destination = temp_dir
            .path()
            .join("playgrounds")
            .join("demo")
            .join(".claude")
            .join("settings.link");
        let metadata = fs::symlink_metadata(&destination).expect("symlink metadata");
        assert!(metadata.file_type().is_symlink());
    }

    #[test]
    fn errors_when_root_config_toml_is_invalid() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            "[playground]\ndefault_agent = ",
        )
        .expect("write invalid root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("invalid root config should fail");

        assert!(error.to_string().contains("failed to parse TOML"));
    }

    #[test]
    fn errors_when_playground_config_toml_is_invalid() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");
        let playground_dir = temp_dir.path().join("playgrounds").join("broken");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(playground_dir.join("apg.toml"), "description = ")
            .expect("write invalid playground config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("invalid playground config should fail");

        assert!(error.to_string().contains("failed to parse TOML"));
    }

    #[test]
    fn errors_when_create_mode_is_invalid() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[playground]
create_mode = "clone"
"#,
        )
        .expect("write invalid root config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("invalid create_mode should fail");

        let message = format!("{error:#}");
        assert!(message.contains("create_mode"));
        assert!(message.contains("clone"));
    }

    #[test]
    fn errors_when_playground_directory_uses_reserved_id() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");
        let playground_dir = temp_dir.path().join("playgrounds").join("default");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(playground_dir.join("apg.toml"), "description = 'reserved'")
            .expect("write playground config");

        let error =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect_err("reserved playground id should fail");
        let message = format!("{error:#}");

        assert!(message.contains("invalid playground directory under"));
        assert!(message.contains("invalid playground id 'default'"));
    }

    #[test]
    fn ignores_non_directory_entries_in_playgrounds_dir() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::write(
            temp_dir.path().join("config.toml"),
            r#"[agent.claude]
cmd = "claude"
"#,
        )
        .expect("write root config");
        let playgrounds_dir = temp_dir.path().join("playgrounds");
        fs::create_dir_all(&playgrounds_dir).expect("create playgrounds dir");
        fs::write(playgrounds_dir.join("README.md"), "ignore me").expect("write file entry");

        let config =
            AppConfig::load_from_paths(ConfigPaths::from_root_dir(temp_dir.path().to_path_buf()))
                .expect("config should load");

        assert!(config.playgrounds.is_empty());
    }

    #[test]
    fn configured_playgrounds_only_returns_valid_initialized_directories() {
        let temp_dir = TempDir::new().expect("temp dir");
        let playgrounds_dir = temp_dir.path().join("playgrounds");
        fs::create_dir_all(&playgrounds_dir).expect("create playgrounds dir");

        let demo_dir = playgrounds_dir.join("demo");
        fs::create_dir_all(&demo_dir).expect("create demo");
        fs::write(demo_dir.join("apg.toml"), "description = 'Demo'").expect("write demo config");

        let ops_dir = playgrounds_dir.join("ops");
        fs::create_dir_all(&ops_dir).expect("create ops");
        fs::write(ops_dir.join("apg.toml"), "description = 'Ops'").expect("write ops config");

        fs::create_dir_all(playgrounds_dir.join("broken")).expect("create broken");
        fs::create_dir_all(playgrounds_dir.join("default")).expect("create reserved");
        fs::create_dir_all(playgrounds_dir.join("invalid")).expect("create invalid");
        fs::write(
            playgrounds_dir.join("invalid").join("apg.toml"),
            "description = ",
        )
        .expect("write invalid config");
        fs::write(playgrounds_dir.join("README.md"), "ignore me").expect("write file");

        assert_eq!(
            configured_playgrounds_at(&playgrounds_dir).expect("list playgrounds"),
            vec![
                ConfiguredPlayground {
                    id: "demo".to_string(),
                    description: "Demo".to_string(),
                },
                ConfiguredPlayground {
                    id: "ops".to_string(),
                    description: "Ops".to_string(),
                }
            ]
        );
    }

    #[test]
    fn user_config_dir_uses_dot_config_on_all_platforms() {
        let base_dir = user_config_base_dir().expect("user config base dir");
        let paths = ConfigPaths::from_user_config_dir().expect("user config paths");

        assert!(base_dir.ends_with(".config"));
        assert_eq!(paths.root_dir, base_dir.join(APP_CONFIG_DIR));
    }

    #[test]
    fn root_config_schema_matches_file_shape() {
        let schema = serde_json::to_value(RootConfigFile::json_schema()).expect("schema json");

        assert_eq!(schema["type"], Value::String("object".to_string()));
        assert!(schema["properties"]["agent"].is_object());
        assert_eq!(
            schema["properties"]["agent"]["additionalProperties"]["$ref"],
            Value::String("#/$defs/AgentConfigFile".to_string())
        );
        assert!(schema["$defs"]["AgentConfigFile"]["properties"]["cmd"].is_object());
        assert!(schema["$defs"]["AgentConfigFile"]["properties"]["config_dir"].is_object());
        assert!(schema["properties"]["default_playground"].is_object());
        assert!(schema["properties"]["saved_playgrounds_dir"].is_object());
        assert!(schema["properties"]["playground"].is_object());
    }

    #[test]
    fn playground_config_schema_matches_file_shape() {
        let schema =
            serde_json::to_value(PlaygroundConfigFile::json_schema()).expect("schema json");

        assert_eq!(schema["type"], Value::String("object".to_string()));
        assert!(schema["properties"]["description"].is_object());
        assert!(schema["properties"]["default_agent"].is_object());
        assert!(schema["properties"]["load_env"].is_object());
        assert!(schema["properties"]["create_mode"].is_object());
        assert_eq!(
            schema["required"],
            Value::Array(vec![Value::String("description".to_string())])
        );
    }
}

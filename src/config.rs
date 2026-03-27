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
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

const APP_CONFIG_DIR: &str = "agent-playground";
const ROOT_CONFIG_FILE_NAME: &str = "config.toml";
const PLAYGROUND_CONFIG_FILE_NAME: &str = "apg.toml";
const PLAYGROUNDS_DIR_NAME: &str = "playgrounds";
const DEFAULT_SUBCOMMAND_PLAYGROUND_ID: &str = "default";
const DEFAULT_SAVED_PLAYGROUNDS_DIR_NAME: &str = "saved-playgrounds";
static TEMPLATE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

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
    /// Agent identifier to shell command mapping from `[agent]`.
    pub agents: BTreeMap<String, String>,
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

        Ok(Self {
            paths,
            agents,
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
    /// Agent template ids that were copied into the playground directory.
    pub initialized_agent_templates: Vec<String>,
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
/// Serializable model for the root `config.toml` file.
pub struct RootConfigFile {
    /// Agent id to command mapping under `[agent]`.
    #[serde(default)]
    pub agent: BTreeMap<String, String>,
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
    agents: BTreeMap<String, String>,
    saved_playgrounds_dir: PathBuf,
    playground_defaults: PlaygroundConfig,
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

impl RootConfigFile {
    /// Returns a JSON Schema for the root config file format.
    pub fn json_schema() -> Schema {
        schema_for!(Self)
    }

    fn defaults_for_paths(paths: &ConfigPaths) -> Self {
        let mut agent = BTreeMap::new();
        agent.insert("claude".to_string(), "claude".to_string());
        agent.insert("opencode".to_string(), "opencode".to_string());

        Self {
            agent,
            saved_playgrounds_dir: Some(default_saved_playgrounds_dir(paths)),
            playground: PlaygroundConfig::builtin_defaults(),
        }
    }

    fn resolve(self, paths: &ConfigPaths) -> Result<ResolvedRootConfig> {
        let defaults = Self::defaults_for_paths(paths);
        let mut agents = defaults.agent;
        agents.extend(self.agent);

        let saved_playgrounds_dir = self
            .saved_playgrounds_dir
            .or(defaults.saved_playgrounds_dir)
            .context("default root config is missing saved_playgrounds_dir")?;
        let playground_defaults = self.playground.merged_over(&defaults.playground);

        Ok(ResolvedRootConfig {
            agents,
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
/// When `agent_ids` are provided, matching embedded templates are copied
/// to `.<agent_id>/` directories in the playground root.
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
    let selected_agent_templates = select_agent_templates(agent_ids)?;

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
    copy_agent_templates(&playground_dir, &selected_agent_templates)?;
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
        initialized_agent_templates: selected_agent_templates
            .iter()
            .map(|(agent_id, _)| agent_id.clone())
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
    for entry in fs::read_dir(playgrounds_dir)
        .with_context(|| format!("failed to read {}", playgrounds_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to inspect {}", playgrounds_dir.display()))?;
        if !entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?
            .is_dir()
        {
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

fn select_agent_templates(agent_ids: &[String]) -> Result<Vec<(String, &'static Dir<'static>)>> {
    let available_templates = available_agent_templates();
    let available_agent_ids = available_templates.keys().cloned().collect::<Vec<_>>();
    let mut selected_templates = Vec::new();

    for agent_id in agent_ids {
        if selected_templates
            .iter()
            .any(|(selected_agent_id, _)| selected_agent_id == agent_id)
        {
            continue;
        }

        let template_dir = available_templates.get(agent_id).with_context(|| {
            format!(
                "unknown agent template '{agent_id}'. Available templates: {}",
                if available_agent_ids.is_empty() {
                    "(none)".to_string()
                } else {
                    available_agent_ids.join(", ")
                }
            )
        })?;
        selected_templates.push((agent_id.clone(), *template_dir));
    }

    Ok(selected_templates)
}

fn available_agent_templates() -> BTreeMap<String, &'static Dir<'static>> {
    let mut agent_templates = BTreeMap::new();

    for template_dir in TEMPLATE_DIR.dirs() {
        let Some(dir_name) = template_dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let Some(agent_id) = dir_name.strip_prefix('.') else {
            continue;
        };

        if agent_id.is_empty() {
            continue;
        }

        agent_templates.insert(agent_id.to_string(), template_dir);
    }

    agent_templates
}

fn copy_agent_templates(
    playground_dir: &Path,
    agent_templates: &[(String, &'static Dir<'static>)],
) -> Result<()> {
    for (agent_id, template_dir) in agent_templates {
        copy_embedded_dir(template_dir, &playground_dir.join(format!(".{agent_id}")))?;
    }

    Ok(())
}

fn copy_embedded_dir(template_dir: &'static Dir<'static>, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;

    for nested_dir in template_dir.dirs() {
        let nested_dir_name = nested_dir.path().file_name().with_context(|| {
            format!(
                "embedded template path has no name: {}",
                nested_dir.path().display()
            )
        })?;
        copy_embedded_dir(nested_dir, &destination.join(nested_dir_name))?;
    }

    for file in template_dir.files() {
        let file_name = file.path().file_name().with_context(|| {
            format!(
                "embedded template file has no name: {}",
                file.path().display()
            )
        })?;
        let destination_file = destination.join(file_name);
        fs::write(&destination_file, file.contents())
            .with_context(|| format!("failed to write {}", destination_file.display()))?;
    }

    Ok(())
}

fn ensure_root_initialized(paths: &ConfigPaths) -> Result<bool> {
    fs::create_dir_all(&paths.root_dir)
        .with_context(|| format!("failed to create {}", paths.root_dir.display()))?;
    fs::create_dir_all(&paths.playgrounds_dir)
        .with_context(|| format!("failed to create {}", paths.playgrounds_dir.display()))?;

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

fn validate_default_agent_defined(
    agents: &BTreeMap<String, String>,
    default_agent: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(default_agent) = default_agent else {
        bail!("{label} is missing");
    };

    if !agents.contains_key(default_agent) {
        bail!("{label} '{default_agent}' is not defined in [agent]");
    }

    Ok(())
}

fn load_playgrounds(
    playgrounds_dir: &Path,
    agents: &BTreeMap<String, String>,
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

    #[test]
    fn init_creates_root_and_playground_configs_from_file_models() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let result = init_playground_at(paths.clone(), "demo", &[]).expect("init should succeed");

        assert!(result.root_config_created);
        assert!(result.playground_config_created);
        assert!(result.initialized_agent_templates.is_empty());
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
        assert_eq!(config.agents.get("claude"), Some(&"claude".to_string()));
        assert_eq!(config.agents.get("opencode"), Some(&"opencode".to_string()));
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("claude")
        );
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

[agent]
claude = "custom-claude"
codex = "codex --fast"

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
            config.agents.get("claude"),
            Some(&"custom-claude".to_string())
        );
        assert_eq!(config.agents.get("opencode"), Some(&"opencode".to_string()));
        assert_eq!(
            config.agents.get("codex"),
            Some(&"codex --fast".to_string())
        );
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("codex")
        );
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
            r#"[agent]
claude = "claude"
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
        assert_eq!(config.agents.get("claude"), Some(&"claude".to_string()));
        assert_eq!(
            config.playground_defaults.default_agent.as_deref(),
            Some("claude")
        );
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

[agent]
claude = "claude"
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
            r#"[agent]
claude = "claude"
opencode = "opencode"
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
    fn init_copies_selected_agent_templates_into_playground() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec!["claude".to_string(), "codex".to_string()];

        let result =
            init_playground_at(paths, "demo", &selected_agents).expect("init should succeed");
        let playground_dir = temp_dir.path().join("playgrounds").join("demo");

        assert_eq!(
            result.initialized_agent_templates,
            vec!["claude".to_string(), "codex".to_string()]
        );
        assert!(
            playground_dir
                .join(".claude")
                .join("settings.json")
                .is_file()
        );
        assert!(playground_dir.join(".codex").join("config.toml").is_file());
        assert!(!playground_dir.join(".opencode").exists());
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
    fn init_deduplicates_selected_agent_templates() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec![
            "claude".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ];

        let result =
            init_playground_at(paths, "demo", &selected_agents).expect("init should succeed");

        assert_eq!(
            result.initialized_agent_templates,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn init_errors_for_unknown_agent_template_before_creating_playground() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());
        let selected_agents = vec!["missing".to_string()];

        let error = init_playground_at(paths, "demo", &selected_agents)
            .expect_err("unknown agent template should fail");

        assert!(
            error
                .to_string()
                .contains("unknown agent template 'missing'")
        );
        assert!(!temp_dir.path().join("playgrounds").join("demo").exists());
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
            r#"[agent]
claude = "claude"
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
            r#"[agent]
claude = "claude"
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
            r#"[agent]
claude = "claude"
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

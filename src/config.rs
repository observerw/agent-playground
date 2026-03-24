use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

const APP_CONFIG_DIR: &str = "agent-playground";
const ROOT_CONFIG_FILE_NAME: &str = "config.toml";
const PLAYGROUND_CONFIG_FILE_NAME: &str = "apg.toml";
const PLAYGROUNDS_DIR_NAME: &str = "playgrounds";
static TEMPLATE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root_dir: PathBuf,
    pub config_file: PathBuf,
    pub playgrounds_dir: PathBuf,
}

impl ConfigPaths {
    pub fn from_user_config_dir() -> Result<Self> {
        let config_dir = user_config_base_dir()?;

        Ok(Self::from_root_dir(config_dir.join(APP_CONFIG_DIR)))
    }

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
pub struct AppConfig {
    pub paths: ConfigPaths,
    pub agents: BTreeMap<String, String>,
    pub default_agent: String,
    pub saved_playgrounds_dir: PathBuf,
    pub playgrounds: BTreeMap<String, PlaygroundDefinition>,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        Self::load_from_paths(ConfigPaths::from_user_config_dir()?)
    }

    fn load_from_paths(paths: ConfigPaths) -> Result<Self> {
        ensure_root_initialized(&paths)?;
        let resolved_root_config = load_root_config(&paths)?;
        let agents = resolved_root_config.agents;
        let default_agent = resolved_root_config.default_agent;
        let saved_playgrounds_dir = resolve_saved_playgrounds_dir(
            &paths.root_dir,
            resolved_root_config.saved_playgrounds_dir,
        );

        if !agents.contains_key(&default_agent) {
            bail!("default agent '{default_agent}' is not defined in [agent]");
        }

        let playgrounds = load_playgrounds(&paths.playgrounds_dir)?;

        Ok(Self {
            paths,
            agents,
            default_agent,
            saved_playgrounds_dir,
            playgrounds,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitResult {
    pub paths: ConfigPaths,
    pub playground_id: String,
    pub root_config_created: bool,
    pub playground_config_created: bool,
    pub initialized_agent_templates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaygroundDefinition {
    pub id: String,
    pub description: String,
    pub load_env: bool,
    pub directory: PathBuf,
    pub config_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RootConfigFile {
    #[serde(default)]
    pub agent: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_playgrounds_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlaygroundConfigFile {
    pub description: String,
    #[serde(default)]
    pub load_env: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRootConfig {
    agents: BTreeMap<String, String>,
    default_agent: String,
    saved_playgrounds_dir: PathBuf,
}

impl RootConfigFile {
    pub fn json_schema() -> Schema {
        schema_for!(Self)
    }

    fn defaults_for_paths(paths: &ConfigPaths) -> Self {
        let mut agent = BTreeMap::new();
        agent.insert("claude".to_string(), "claude".to_string());
        agent.insert("opencode".to_string(), "opencode".to_string());

        Self {
            agent,
            default_agent: Some("claude".to_string()),
            saved_playgrounds_dir: Some(default_saved_playgrounds_dir(paths)),
        }
    }

    fn resolve(self, paths: &ConfigPaths) -> Result<ResolvedRootConfig> {
        let defaults = Self::defaults_for_paths(paths);
        let mut agents = defaults.agent;
        agents.extend(self.agent);

        let default_agent = self
            .default_agent
            .or(defaults.default_agent)
            .context("default root config is missing default_agent")?;
        let saved_playgrounds_dir = self
            .saved_playgrounds_dir
            .or(defaults.saved_playgrounds_dir)
            .context("default root config is missing saved_playgrounds_dir")?;

        Ok(ResolvedRootConfig {
            agents,
            default_agent,
            saved_playgrounds_dir,
        })
    }
}

impl PlaygroundConfigFile {
    pub fn json_schema() -> Schema {
        schema_for!(Self)
    }

    fn for_playground(playground_id: &str) -> Self {
        Self {
            description: format!("TODO: describe {playground_id}"),
            load_env: false,
        }
    }
}

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

fn default_saved_playgrounds_dir(paths: &ConfigPaths) -> PathBuf {
    paths.root_dir.join("saved-playgrounds")
}

fn resolve_saved_playgrounds_dir(root_dir: &Path, configured_path: PathBuf) -> PathBuf {
    if configured_path.is_absolute() {
        return configured_path;
    }

    root_dir.join(configured_path)
}

fn load_playgrounds(playgrounds_dir: &Path) -> Result<BTreeMap<String, PlaygroundDefinition>> {
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

        playgrounds.insert(
            id.clone(),
            PlaygroundDefinition {
                id,
                description: playground_config.description,
                load_env: playground_config.load_env,
                directory,
                config_file,
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
        APP_CONFIG_DIR, AppConfig, ConfigPaths, PlaygroundConfigFile, RootConfigFile,
        init_playground_at, read_toml_file, user_config_base_dir,
    };
    use serde_json::Value;
    use std::fs;
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
        assert_eq!(config.default_agent, "claude");
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
            !config
                .playgrounds
                .get("demo")
                .expect("demo playground")
                .load_env
        );
    }

    #[test]
    fn merges_root_agents_and_loads_playgrounds() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();
        fs::write(
            root.join("config.toml"),
            r#"default_agent = "codex"
saved_playgrounds_dir = "archives"

[agent]
claude = "custom-claude"
codex = "codex --fast"
"#,
        )
        .expect("write root config");

        let playground_dir = root.join("playgrounds").join("demo");
        fs::create_dir_all(&playground_dir).expect("create playground dir");
        fs::write(
            playground_dir.join("apg.toml"),
            r#"description = "Demo playground"
load_env = true"#,
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
        assert_eq!(config.default_agent, "codex");
        assert_eq!(config.saved_playgrounds_dir, root.join("archives"));

        let playground = config.playgrounds.get("demo").expect("demo playground");
        assert_eq!(playground.description, "Demo playground");
        assert!(playground.load_env);
        assert_eq!(playground.directory, playground_dir);
    }

    #[test]
    fn load_auto_initializes_missing_root_config() {
        let temp_dir = TempDir::new().expect("temp dir");
        let paths = ConfigPaths::from_root_dir(temp_dir.path().to_path_buf());

        let config = AppConfig::load_from_paths(paths).expect("missing root config should init");

        assert!(temp_dir.path().join("config.toml").is_file());
        assert!(temp_dir.path().join("playgrounds").is_dir());
        assert_eq!(config.agents.get("claude"), Some(&"claude".to_string()));
        assert_eq!(config.default_agent, "claude");
        assert_eq!(
            config.saved_playgrounds_dir,
            temp_dir.path().join("saved-playgrounds")
        );
    }

    #[test]
    fn respects_absolute_saved_playgrounds_dir() {
        let temp_dir = TempDir::new().expect("temp dir");
        let archive_dir = TempDir::new().expect("archive dir");
        let archive_path = archive_dir.path().display().to_string();
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
            r#"default_agent = "codex""#,
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
        fs::write(temp_dir.path().join("config.toml"), "default_agent = ")
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
        assert!(schema["properties"]["default_agent"].is_object());
        assert!(schema["properties"]["saved_playgrounds_dir"].is_object());
    }

    #[test]
    fn playground_config_schema_matches_file_shape() {
        let schema =
            serde_json::to_value(PlaygroundConfigFile::json_schema()).expect("schema json");

        assert_eq!(schema["type"], Value::String("object".to_string()));
        assert!(schema["properties"]["description"].is_object());
        assert!(schema["properties"]["load_env"].is_object());
        assert_eq!(
            schema["required"],
            Value::Array(vec![Value::String("description".to_string())])
        );
    }
}

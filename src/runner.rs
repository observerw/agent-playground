use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command as ProcessCommand},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use dotenvy::Error as DotenvError;
use tempfile::tempdir;

use crate::config::{AppConfig, PlaygroundDefinition};

const DOTENV_FILE_NAME: &str = ".env";

pub fn run_playground(
    config: &AppConfig,
    playground_id: &str,
    selected_agent_id: Option<&str>,
    save_on_exit: bool,
) -> Result<i32> {
    let playground = config
        .playgrounds
        .get(playground_id)
        .with_context(|| format!("unknown playground '{playground_id}'"))?;
    let agent_id = selected_agent_id.unwrap_or(&config.default_agent);
    let agent_command = config
        .agents
        .get(agent_id)
        .with_context(|| format!("unknown agent '{agent_id}'"))?;

    let temp_dir = tempdir().context("failed to create temporary playground directory")?;
    copy_playground_contents(playground, temp_dir.path())?;
    let playground_env = load_playground_env(playground)?;

    let status = build_agent_command(agent_command)
        .envs(playground_env)
        .current_dir(temp_dir.path())
        .status()
        .with_context(|| format!("failed to start agent '{agent_id}'"))?;

    let (exit_code, exited_normally) = exit_code_from_status(status)?;

    if should_save_playground_snapshot(exited_normally, save_on_exit) {
        let saved_path = save_playground_snapshot(
            temp_dir.path(),
            &config.saved_playgrounds_dir,
            playground_id,
        )?;
        println!("saved playground snapshot to {}", saved_path.display());
    }

    Ok(exit_code)
}

fn should_save_playground_snapshot(exited_normally: bool, save_on_exit: bool) -> bool {
    exited_normally && save_on_exit
}

fn save_playground_snapshot(
    source_dir: &Path,
    saved_playgrounds_dir: &Path,
    playground_id: &str,
) -> Result<PathBuf> {
    fs::create_dir_all(saved_playgrounds_dir)
        .with_context(|| format!("failed to create {}", saved_playgrounds_dir.display()))?;

    let destination = next_saved_playground_dir(saved_playgrounds_dir, playground_id);
    fs::create_dir_all(&destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    copy_directory_contents(source_dir, &destination)?;

    Ok(destination)
}

fn next_saved_playground_dir(saved_playgrounds_dir: &Path, playground_id: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let base_name = format!("{playground_id}-{timestamp}");
    let mut candidate = saved_playgrounds_dir.join(&base_name);
    let mut suffix = 1;

    while candidate.exists() {
        candidate = saved_playgrounds_dir.join(format!("{base_name}-{suffix}"));
        suffix += 1;
    }

    candidate
}

fn copy_playground_contents(playground: &PlaygroundDefinition, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(&playground.directory)
        .with_context(|| format!("failed to read {}", playground.directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect an entry under {}",
                playground.directory.display()
            )
        })?;
        let source_path = entry.path();

        if should_skip_playground_path(playground, &source_path) {
            continue;
        }

        copy_path(&source_path, &destination.join(entry.file_name()))?;
    }

    Ok(())
}

fn should_skip_playground_path(playground: &PlaygroundDefinition, source_path: &Path) -> bool {
    source_path == playground.config_file
        || (playground.load_env
            && source_path
                .file_name()
                .is_some_and(|name| name == DOTENV_FILE_NAME))
}

fn load_playground_env(playground: &PlaygroundDefinition) -> Result<Vec<(String, String)>> {
    if !playground.load_env {
        return Ok(Vec::new());
    }

    let env_path = playground.directory.join(DOTENV_FILE_NAME);
    match dotenvy::from_path_iter(&env_path) {
        Ok(entries) => entries
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("failed to parse {}", env_path.display())),
        Err(DotenvError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Vec::new())
        }
        Err(error) => Err(error).with_context(|| format!("failed to load {}", env_path.display())),
    }
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to inspect an entry under {}", source.display()))?;
        copy_path(&entry.path(), &destination.join(entry.file_name()))?;
    }

    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect {}", source.display()))?;

    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;

        for entry in
            fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
        {
            let entry = entry.with_context(|| {
                format!("failed to inspect an entry under {}", source.display())
            })?;
            copy_path(&entry.path(), &destination.join(entry.file_name()))?;
        }

        return Ok(());
    }

    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        fs::copy(source, destination).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }

    bail!(
        "unsupported file type while copying playground contents: {}",
        source.display()
    );
}

fn build_agent_command(agent_command: &str) -> ProcessCommand {
    #[cfg(windows)]
    {
        let mut command = ProcessCommand::new("cmd");
        command.arg("/C").arg(agent_command);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = ProcessCommand::new("sh");
        command.arg("-c").arg(agent_command);
        command
    }
}

fn exit_code_from_status(status: process::ExitStatus) -> Result<(i32, bool)> {
    if let Some(code) = status.code() {
        return Ok((code, code == 0));
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            return Ok((128 + signal, false));
        }
    }

    bail!("agent process ended without an exit code")
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::config::{AppConfig, ConfigPaths, PlaygroundDefinition};

    use super::{
        copy_playground_contents, exit_code_from_status, run_playground, save_playground_snapshot,
        should_save_playground_snapshot,
    };

    #[cfg(unix)]
    fn command_writing_marker(marker: &str) -> String {
        format!("printf '{marker}' > agent.txt && test ! -e apg.toml")
    }

    #[cfg(windows)]
    fn command_writing_marker(marker: &str) -> String {
        format!("echo {marker}>agent.txt && if exist apg.toml exit /b 1")
    }

    #[cfg(unix)]
    fn failing_command() -> String {
        "printf 'failed' > agent.txt; exit 7".to_string()
    }

    #[cfg(windows)]
    fn failing_command() -> String {
        "echo failed>agent.txt & exit /b 7".to_string()
    }

    #[cfg(unix)]
    fn command_recording_env(var_name: &str) -> String {
        format!("printf '%s' \"${var_name}\" > env.txt && test ! -e .env && test ! -e apg.toml")
    }

    #[cfg(windows)]
    fn command_recording_env(var_name: &str) -> String {
        format!(
            "<nul set /p=%{var_name}%>env.txt && if exist .env exit /b 1 && if exist apg.toml exit /b 1"
        )
    }

    fn single_saved_snapshot(save_root: &Path) -> Result<std::path::PathBuf> {
        let snapshots = fs::read_dir(save_root)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        assert_eq!(snapshots.len(), 1);
        Ok(snapshots.into_iter().next().expect("single snapshot"))
    }

    fn make_playground(source_dir: &Path, playground_id: &str) -> Result<PlaygroundDefinition> {
        let config_file = source_dir.join("apg.toml");
        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(source_dir.join("notes.txt"), "hello")?;

        Ok(PlaygroundDefinition {
            id: playground_id.to_string(),
            description: "demo".to_string(),
            load_env: false,
            directory: source_dir.to_path_buf(),
            config_file,
        })
    }

    fn make_config(
        source_dir: &Path,
        save_root: &Path,
        playground_id: &str,
        default_agent: &str,
        agents: &[(&str, String)],
    ) -> Result<AppConfig> {
        let playground = make_playground(source_dir, playground_id)?;
        let agents = agents
            .iter()
            .map(|(id, command)| ((*id).to_string(), command.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut playgrounds = BTreeMap::new();
        playgrounds.insert(playground_id.to_string(), playground);

        Ok(AppConfig {
            paths: ConfigPaths::from_root_dir(source_dir.join("config-root")),
            agents,
            default_agent: default_agent.to_string(),
            saved_playgrounds_dir: save_root.to_path_buf(),
            playgrounds,
        })
    }

    #[test]
    fn copies_playground_contents_except_config_file() -> Result<()> {
        let source_dir = tempdir()?;
        let destination_dir = tempdir()?;
        let nested_dir = source_dir.path().join("nested");
        let config_file = source_dir.path().join("apg.toml");
        let note_file = source_dir.path().join("notes.txt");
        let nested_file = nested_dir.join("task.md");

        fs::create_dir_all(&nested_dir)?;
        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(&note_file, "hello")?;
        fs::write(&nested_file, "nested")?;

        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "demo".to_string(),
            load_env: false,
            directory: source_dir.path().to_path_buf(),
            config_file: config_file.clone(),
        };

        copy_playground_contents(&playground, destination_dir.path())?;

        assert!(!destination_dir.path().join("apg.toml").exists());
        assert_eq!(
            fs::read_to_string(destination_dir.path().join("notes.txt"))?,
            "hello"
        );
        assert_eq!(
            fs::read_to_string(destination_dir.path().join("nested").join("task.md"))?,
            "nested"
        );

        Ok(())
    }

    #[test]
    fn skips_dotenv_file_when_load_env_is_enabled() -> Result<()> {
        let source_dir = tempdir()?;
        let destination_dir = tempdir()?;
        let config_file = source_dir.path().join("apg.toml");
        let env_file = source_dir.path().join(".env");

        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(source_dir.path().join("notes.txt"), "hello")?;
        fs::write(&env_file, "API_TOKEN=secret\n")?;

        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "demo".to_string(),
            load_env: true,
            directory: source_dir.path().to_path_buf(),
            config_file,
        };

        copy_playground_contents(&playground, destination_dir.path())?;

        assert!(!destination_dir.path().join(".env").exists());
        assert_eq!(
            fs::read_to_string(destination_dir.path().join("notes.txt"))?,
            "hello"
        );

        Ok(())
    }

    #[test]
    fn saves_snapshot_only_for_normal_exit_when_enabled() {
        assert!(should_save_playground_snapshot(true, true));
        assert!(!should_save_playground_snapshot(true, false));
        assert!(!should_save_playground_snapshot(false, true));
        assert!(!should_save_playground_snapshot(false, false));
    }

    #[test]
    fn saves_temporary_playground_snapshot() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let nested_dir = source_dir.path().join("nested");

        fs::create_dir_all(&nested_dir)?;
        fs::write(source_dir.path().join("notes.txt"), "hello")?;
        fs::write(nested_dir.join("task.md"), "nested")?;

        let saved_path = save_playground_snapshot(source_dir.path(), save_root.path(), "demo")?;

        assert!(saved_path.starts_with(save_root.path()));
        assert_eq!(fs::read_to_string(saved_path.join("notes.txt"))?, "hello");
        assert_eq!(
            fs::read_to_string(saved_path.join("nested").join("task.md"))?,
            "nested"
        );

        Ok(())
    }

    #[test]
    fn only_zero_exit_code_counts_as_normal_exit() -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            let success = std::process::ExitStatus::from_raw(0);
            let interrupted = std::process::ExitStatus::from_raw(130 << 8);

            assert_eq!(exit_code_from_status(success)?, (0, true));
            assert_eq!(exit_code_from_status(interrupted)?, (130, false));
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;

            let success = std::process::ExitStatus::from_raw(0);
            let interrupted = std::process::ExitStatus::from_raw(130);

            assert_eq!(exit_code_from_status(success)?, (0, true));
            assert_eq!(exit_code_from_status(interrupted)?, (130, false));
        }

        Ok(())
    }

    #[test]
    fn errors_for_unknown_playground() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", command_writing_marker("default"))],
        )?;

        let error =
            run_playground(&config, "missing", None, false).expect_err("unknown playground");

        assert!(error.to_string().contains("unknown playground 'missing'"));
        Ok(())
    }

    #[test]
    fn errors_for_unknown_agent() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", command_writing_marker("default"))],
        )?;

        let error =
            run_playground(&config, "demo", Some("missing"), false).expect_err("unknown agent");

        assert!(error.to_string().contains("unknown agent 'missing'"));
        Ok(())
    }

    #[test]
    fn uses_default_agent_and_saves_snapshot_when_enabled() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", command_writing_marker("default"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, true)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "default"
        );
        assert_eq!(fs::read_to_string(snapshot.join("notes.txt"))?, "hello");
        assert!(!snapshot.join("apg.toml").exists());
        Ok(())
    }

    #[test]
    fn selected_agent_overrides_default_agent() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[
                ("claude", command_writing_marker("default")),
                ("codex", command_writing_marker("selected")),
            ],
        )?;

        let exit_code = run_playground(&config, "demo", Some("codex"), true)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "selected"
        );
        Ok(())
    }

    #[test]
    fn does_not_save_snapshot_when_disabled() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", command_writing_marker("default"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, false)?;

        assert_eq!(exit_code, 0);
        assert_eq!(fs::read_dir(save_root.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn does_not_save_snapshot_when_agent_exits_with_error() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", failing_command())],
        )?;

        let exit_code = run_playground(&config, "demo", None, true)?;

        assert_eq!(exit_code, 7);
        assert_eq!(fs::read_dir(save_root.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn loads_dotenv_into_agent_environment_without_copying_file() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        fs::write(
            source_dir.path().join(".env"),
            "PLAYGROUND_SECRET=token-123\n",
        )?;
        let mut config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            "claude",
            &[("claude", command_recording_env("PLAYGROUND_SECRET"))],
        )?;
        config
            .playgrounds
            .get_mut("demo")
            .expect("demo playground")
            .load_env = true;

        let exit_code = run_playground(&config, "demo", None, true)?;
        let snapshot = single_saved_snapshot(save_root.path())?;
        assert_eq!(exit_code, 0);
        assert_eq!(fs::read_to_string(snapshot.join("env.txt"))?, "token-123");
        assert!(!snapshot.join(".env").exists());
        Ok(())
    }
}

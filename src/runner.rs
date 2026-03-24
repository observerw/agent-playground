use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command as ProcessCommand},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use tempfile::tempdir;

use crate::config::{AppConfig, PlaygroundDefinition};

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

    let status = build_agent_command(agent_command)
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

        if source_path == playground.config_file {
            continue;
        }

        copy_path(&source_path, &destination.join(entry.file_name()))?;
    }

    Ok(())
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
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::config::PlaygroundDefinition;

    use super::{
        copy_playground_contents, exit_code_from_status, save_playground_snapshot,
        should_save_playground_snapshot,
    };

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
}

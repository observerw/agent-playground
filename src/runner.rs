//! Runtime execution for launching an agent inside a temporary playground.
//!
//! The runner materializes playground files into a throwaway directory, executes
//! the selected agent command in that directory, and optionally persists the
//! final state as a snapshot.

use std::{
    collections::HashSet,
    fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{self, Command as ProcessCommand},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use dotenvy::Error as DotenvError;
use tempfile::tempdir;

use crate::config::{AppConfig, CreateMode, PlaygroundDefinition};
use crate::utils::symlink::{apply_directory_mounts, copy_symlink};

pub use crate::utils::symlink::DirectoryMount;

const DOTENV_FILE_NAME: &str = ".env";
const DEFAULT_PLAYGROUND_ID: &str = "__default__";

struct RunContext<'a> {
    playground_env: Vec<(String, String)>,
    save_on_exit: bool,
    in_place_mode: bool,
    saved_playgrounds_dir: &'a Path,
    playground_id: &'a str,
    mounts: &'a [DirectoryMount],
}

/// Runs a configured playground with the selected agent command.
///
/// The execution flow is:
///
/// 1. Resolve the playground and agent command from [`AppConfig`].
/// 2. Materialize playground contents into a temporary directory.
/// 3. Optionally load `.env` key-value pairs into the child process.
/// 4. Run the agent command in the temporary directory.
/// 5. Optionally save a snapshot of that directory on normal exit.
///
/// When `selected_agent_id` is `None`, the module falls back to the
/// playground default agent and then to the root default agent.
///
/// # Returns
///
/// Returns the exit code that should be used by the caller process.
///
/// # Errors
///
/// Returns an error if configuration references are invalid, filesystem
/// operations fail, environment parsing fails, or the agent process cannot be
/// started or yields an unrepresentable status.
pub fn run_playground(
    config: &AppConfig,
    playground_id: &str,
    selected_agent_id: Option<&str>,
    save_on_exit: bool,
    mounts: &[DirectoryMount],
    in_path: Option<&Path>,
) -> Result<i32> {
    if let Some(in_path) = in_path {
        return run_playground_in_dir(
            config,
            playground_id,
            selected_agent_id,
            in_path,
            save_on_exit,
            mounts,
        );
    }

    let playground = config
        .playgrounds
        .get(playground_id)
        .with_context(|| format!("unknown playground '{playground_id}'"))?;
    let playground_config = config.resolve_playground_config(playground)?;
    let agent_id = selected_agent_id.unwrap_or(&playground_config.default_agent);
    let agent_command = config
        .agents
        .get(agent_id)
        .map(|agent| agent.cmd.as_str())
        .with_context(|| format!("unknown agent '{agent_id}'"))?;
    let load_env = playground_config.load_env;
    let create_mode = playground_config.create_mode;

    let temp_dir = tempdir().context("failed to create temporary playground directory")?;
    materialize_playground_contents(playground, load_env, create_mode, temp_dir.path())?;
    apply_directory_mounts(temp_dir.path(), mounts)?;
    let playground_env = load_playground_env(playground, load_env)?;

    run_agent_in_directory(
        temp_dir.path(),
        agent_id,
        agent_command,
        RunContext {
            playground_env,
            save_on_exit,
            in_place_mode: false,
            saved_playgrounds_dir: &config.saved_playgrounds_dir,
            playground_id,
            mounts,
        },
    )
}

/// Runs the selected agent inside an empty temporary playground directory.
///
/// This is similar to [`run_playground`], but it does not require a configured
/// playground template and starts from an empty working directory instead.
pub fn run_default_playground(
    config: &AppConfig,
    selected_agent_id: Option<&str>,
    save_on_exit: bool,
    mounts: &[DirectoryMount],
) -> Result<i32> {
    let default_agent = config
        .playground_defaults
        .default_agent
        .as_deref()
        .context("default playground config is missing default_agent")?;
    let agent_id = selected_agent_id.unwrap_or(default_agent);
    let agent_command = config
        .agents
        .get(agent_id)
        .map(|agent| agent.cmd.as_str())
        .with_context(|| format!("unknown agent '{agent_id}'"))?;

    let temp_dir = tempdir().context("failed to create temporary playground directory")?;
    apply_directory_mounts(temp_dir.path(), mounts)?;

    run_agent_in_directory(
        temp_dir.path(),
        agent_id,
        agent_command,
        RunContext {
            playground_env: Vec::new(),
            save_on_exit,
            in_place_mode: false,
            saved_playgrounds_dir: &config.saved_playgrounds_dir,
            playground_id: DEFAULT_PLAYGROUND_ID,
            mounts,
        },
    )
}

/// Runs a configured playground directly in an existing directory by
/// temporarily injecting symlinks and cleaning them up after the agent exits.
pub fn run_playground_in_dir(
    config: &AppConfig,
    playground_id: &str,
    selected_agent_id: Option<&str>,
    in_path: &Path,
    save_on_exit: bool,
    mounts: &[DirectoryMount],
) -> Result<i32> {
    let playground = config
        .playgrounds
        .get(playground_id)
        .with_context(|| format!("unknown playground '{playground_id}'"))?;
    let playground_config = config.resolve_playground_config(playground)?;
    let agent_id = selected_agent_id.unwrap_or(&playground_config.default_agent);
    let agent_command = config
        .agents
        .get(agent_id)
        .map(|agent| agent.cmd.as_str())
        .with_context(|| format!("unknown agent '{agent_id}'"))?;
    let load_env = playground_config.load_env;
    let playground_env = load_playground_env(playground, load_env)?;

    let working_dir = prepare_in_place_directory(in_path)?;
    let mut link_session = LinkSession::default();
    let run_result = (|| {
        link_playground_into_directory(playground, load_env, working_dir, &mut link_session)?;
        apply_directory_mounts_in_place(working_dir, mounts, &mut link_session)?;
        run_agent_in_directory(
            working_dir,
            agent_id,
            agent_command,
            RunContext {
                playground_env,
                save_on_exit,
                in_place_mode: true,
                saved_playgrounds_dir: &config.saved_playgrounds_dir,
                playground_id,
                mounts,
            },
        )
    })();
    let cleanup_result = link_session.cleanup();

    resolve_run_and_cleanup_result(run_result, cleanup_result)
}

/// Runs an empty default playground directly in an existing directory by
/// temporarily mounting requested paths and cleaning them up after exit.
pub fn run_default_playground_in_dir(
    config: &AppConfig,
    selected_agent_id: Option<&str>,
    in_path: &Path,
    save_on_exit: bool,
    mounts: &[DirectoryMount],
) -> Result<i32> {
    let default_agent = config
        .playground_defaults
        .default_agent
        .as_deref()
        .context("default playground config is missing default_agent")?;
    let agent_id = selected_agent_id.unwrap_or(default_agent);
    let agent_command = config
        .agents
        .get(agent_id)
        .map(|agent| agent.cmd.as_str())
        .with_context(|| format!("unknown agent '{agent_id}'"))?;
    let working_dir = prepare_in_place_directory(in_path)?;
    let mut link_session = LinkSession::default();
    let run_result = (|| {
        apply_directory_mounts_in_place(working_dir, mounts, &mut link_session)?;
        run_agent_in_directory(
            working_dir,
            agent_id,
            agent_command,
            RunContext {
                playground_env: Vec::new(),
                save_on_exit,
                in_place_mode: true,
                saved_playgrounds_dir: &config.saved_playgrounds_dir,
                playground_id: DEFAULT_PLAYGROUND_ID,
                mounts,
            },
        )
    })();
    let cleanup_result = link_session.cleanup();

    resolve_run_and_cleanup_result(run_result, cleanup_result)
}

fn run_agent_in_directory(
    working_dir: &Path,
    agent_id: &str,
    agent_command: &str,
    run_context: RunContext<'_>,
) -> Result<i32> {
    let status = build_agent_command(agent_command)
        .envs(run_context.playground_env)
        .current_dir(working_dir)
        .status()
        .with_context(|| format!("failed to start agent '{agent_id}'"))?;

    let (exit_code, exited_normally) = exit_code_from_status(status)?;

    let should_save = !run_context.in_place_mode
        && (should_save_playground_snapshot(exited_normally, run_context.save_on_exit)
            || (should_prompt_to_save_playground_snapshot(
                exited_normally,
                run_context.save_on_exit,
                is_interactive_terminal(),
            ) && prompt_to_save_playground_snapshot(
                io::stdin().lock(),
                &mut io::stdout().lock(),
            )?));

    if should_save {
        let saved_path = save_playground_snapshot(
            working_dir,
            run_context.saved_playgrounds_dir,
            run_context.playground_id,
            mounted_paths(working_dir, run_context.mounts),
        )?;
        println!("saved playground snapshot to {}", saved_path.display());
    }

    Ok(exit_code)
}

fn mounted_paths(working_dir: &Path, mounts: &[DirectoryMount]) -> HashSet<PathBuf> {
    mounts
        .iter()
        .map(|mount| working_dir.join(&mount.destination))
        .collect()
}

#[derive(Debug, Default)]
struct LinkSession {
    created_symlinks: Vec<PathBuf>,
    created_symlink_set: HashSet<PathBuf>,
    created_directories: Vec<PathBuf>,
    created_directory_set: HashSet<PathBuf>,
}

impl LinkSession {
    fn record_symlink(&mut self, path: PathBuf) {
        if self.created_symlink_set.insert(path.clone()) {
            self.created_symlinks.push(path);
        }
    }

    fn record_directory(&mut self, path: PathBuf) {
        if self.created_directory_set.insert(path.clone()) {
            self.created_directories.push(path);
        }
    }

    fn cleanup(self) -> Result<()> {
        let mut errors = Vec::new();

        let mut symlinks = self.created_symlinks;
        symlinks.sort_by_key(|path| std::cmp::Reverse(path_depth(path)));
        for symlink in symlinks {
            if let Err(error) = remove_symlink_if_present(&symlink) {
                errors.push(format!(
                    "failed to remove symlink {}: {error:#}",
                    symlink.display()
                ));
            }
        }

        let mut directories = self.created_directories;
        directories.sort_by_key(|path| std::cmp::Reverse(path_depth(path)));
        for directory in directories {
            if let Err(error) = remove_directory_if_empty(&directory) {
                errors.push(format!(
                    "failed to remove empty directory {}: {error:#}",
                    directory.display()
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            bail!("{}", errors.join("; "));
        }
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn remove_symlink_if_present(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if !metadata.file_type().is_symlink() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }

    #[cfg(windows)]
    {
        match fs::remove_dir(path) {
            Ok(()) => {}
            Err(dir_error) => {
                fs::remove_file(path)
                    .map_err(|_| dir_error)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
        }
    }

    Ok(())
}

fn remove_directory_if_empty(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }

    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::DirectoryNotEmpty =>
        {
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let metadata = fs::metadata(path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.is_dir() {
                return Ok(());
            }

            bail!("in_path '{}' exists but is not a directory", path.display());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }

    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn prepare_in_place_directory(in_path: &Path) -> Result<&Path> {
    ensure_directory(in_path)?;
    Ok(in_path)
}

fn resolve_run_and_cleanup_result(
    run_result: Result<i32>,
    cleanup_result: Result<()>,
) -> Result<i32> {
    match run_result {
        Ok(exit_code) => {
            cleanup_result?;
            Ok(exit_code)
        }
        Err(run_error) => {
            if let Err(cleanup_error) = cleanup_result {
                return Err(run_error.context(format!(
                    "failed to clean up in_path links: {cleanup_error:#}"
                )));
            }

            Err(run_error)
        }
    }
}

fn link_playground_into_directory(
    playground: &PlaygroundDefinition,
    load_env: bool,
    destination_root: &Path,
    link_session: &mut LinkSession,
) -> Result<()> {
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

        if should_skip_playground_path(playground, load_env, &source_path) {
            continue;
        }

        link_path_into_destination(
            &source_path,
            &destination_root.join(entry.file_name()),
            link_session,
        )?;
    }

    Ok(())
}

fn link_path_into_destination(
    source: &Path,
    destination: &Path,
    link_session: &mut LinkSession,
) -> Result<()> {
    let source_metadata = inspect_path(source, true)?;
    match fs::symlink_metadata(destination) {
        Ok(destination_metadata) => {
            if source_metadata.is_dir()
                && destination_metadata.is_dir()
                && !destination_metadata.file_type().is_symlink()
            {
                for entry in fs::read_dir(source)
                    .with_context(|| format!("failed to read {}", source.display()))?
                {
                    let entry = entry.with_context(|| {
                        format!("failed to inspect an entry under {}", source.display())
                    })?;
                    link_path_into_destination(
                        &entry.path(),
                        &destination.join(entry.file_name()),
                        link_session,
                    )?;
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_parent_directories(destination, link_session)?;
            create_symlink(source, destination, source_metadata.is_dir()).with_context(|| {
                format!(
                    "failed to symlink {} to {}",
                    source.display(),
                    destination.display()
                )
            })?;
            link_session.record_symlink(destination.to_path_buf());
            Ok(())
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", destination.display()))
        }
    }
}

fn ensure_parent_directories(path: &Path, link_session: &mut LinkSession) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let is_directory = if metadata.file_type().is_symlink() {
                    fs::metadata(&current)
                        .with_context(|| format!("failed to inspect {}", current.display()))?
                        .is_dir()
                } else {
                    metadata.is_dir()
                };

                if is_directory {
                    continue;
                }

                bail!(
                    "cannot create {} because {} is not a directory",
                    parent.display(),
                    current.display()
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("failed to create {}", current.display()))?;
                link_session.record_directory(current.clone());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }

    Ok(())
}

fn apply_directory_mounts_in_place(
    working_dir: &Path,
    mounts: &[DirectoryMount],
    link_session: &mut LinkSession,
) -> Result<()> {
    for mount in mounts {
        let destination = working_dir.join(&mount.destination);
        ensure_destination_absent(&destination)?;
        ensure_parent_directories(&destination, link_session)?;
        create_symlink(&mount.source, &destination, true).with_context(|| {
            format!(
                "failed to mount {} at {}",
                mount.source.display(),
                destination.display()
            )
        })?;
        link_session.record_symlink(destination);
    }

    Ok(())
}

fn ensure_destination_absent(destination: &Path) -> Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        bail!(
            "mount destination already exists inside playground: {}",
            destination.display()
        );
    }

    Ok(())
}

fn should_save_playground_snapshot(exited_normally: bool, save_on_exit: bool) -> bool {
    exited_normally && save_on_exit
}

fn should_prompt_to_save_playground_snapshot(
    exited_normally: bool,
    save_on_exit: bool,
    is_interactive: bool,
) -> bool {
    exited_normally && !save_on_exit && is_interactive
}

fn is_interactive_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn prompt_to_save_playground_snapshot<R: BufRead, W: Write>(
    mut input: R,
    output: &mut W,
) -> Result<bool> {
    write!(output, "Keep temporary playground copy? [y/N] ")
        .context("failed to write save prompt")?;
    output.flush().context("failed to flush save prompt")?;

    let mut response = String::new();
    input
        .read_line(&mut response)
        .context("failed to read save prompt response")?;

    let normalized = response.trim().to_ascii_lowercase();
    Ok(matches!(normalized.as_str(), "y" | "yes"))
}

fn save_playground_snapshot(
    source_dir: &Path,
    saved_playgrounds_dir: &Path,
    playground_id: &str,
    preserved_symlink_paths: HashSet<PathBuf>,
) -> Result<PathBuf> {
    fs::create_dir_all(saved_playgrounds_dir)
        .with_context(|| format!("failed to create {}", saved_playgrounds_dir.display()))?;

    let destination = next_saved_playground_dir(saved_playgrounds_dir, playground_id);
    fs::create_dir_all(&destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    copy_directory_contents(source_dir, &destination, &preserved_symlink_paths)?;

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

fn materialize_playground_contents(
    playground: &PlaygroundDefinition,
    load_env: bool,
    create_mode: CreateMode,
    destination: &Path,
) -> Result<()> {
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

        if should_skip_playground_path(playground, load_env, &source_path) {
            continue;
        }

        materialize_path(
            &source_path,
            &destination.join(entry.file_name()),
            create_mode,
        )?;
    }

    Ok(())
}

fn materialize_path(source: &Path, destination: &Path, create_mode: CreateMode) -> Result<()> {
    match create_mode {
        CreateMode::Copy => copy_path(source, destination),
        CreateMode::Symlink => symlink_path(source, destination),
        CreateMode::Hardlink => hardlink_path(source, destination),
    }
}

fn should_skip_playground_path(
    playground: &PlaygroundDefinition,
    load_env: bool,
    source_path: &Path,
) -> bool {
    source_path == playground.config_file
        || (load_env
            && source_path
                .file_name()
                .is_some_and(|name| name == DOTENV_FILE_NAME))
}

fn load_playground_env(
    playground: &PlaygroundDefinition,
    load_env: bool,
) -> Result<Vec<(String, String)>> {
    if !load_env {
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

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    preserved_symlink_paths: &HashSet<PathBuf>,
) -> Result<()> {
    let mut active_directories = HashSet::new();
    copy_directory_contents_following_symlinks(
        source,
        destination,
        preserved_symlink_paths,
        &mut active_directories,
    )
}

fn copy_directory_contents_following_symlinks(
    source: &Path,
    destination: &Path,
    preserved_symlink_paths: &HashSet<PathBuf>,
    active_directories: &mut HashSet<PathBuf>,
) -> Result<()> {
    let canonical_source = fs::canonicalize(source)
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    if !active_directories.insert(canonical_source.clone()) {
        bail!(
            "refusing to save playground snapshot because symlink traversal would revisit {}",
            canonical_source.display()
        );
    }

    let result = copy_directory_entries_following_symlinks(
        source,
        destination,
        preserved_symlink_paths,
        active_directories,
    );
    active_directories.remove(&canonical_source);
    result
}

fn copy_directory_entries_following_symlinks(
    source: &Path,
    destination: &Path,
    preserved_symlink_paths: &HashSet<PathBuf>,
    active_directories: &mut HashSet<PathBuf>,
) -> Result<()> {
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to inspect an entry under {}", source.display()))?;
        copy_path_following_symlinks(
            &entry.path(),
            &destination.join(entry.file_name()),
            preserved_symlink_paths,
            active_directories,
        )?;
    }

    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> Result<()> {
    copy_path_with_symlink_behavior(source, destination, false)
}

fn copy_path_with_symlink_behavior(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<()> {
    let metadata = inspect_path(source, follow_symlinks)?;

    if metadata.file_type().is_symlink() {
        copy_symlink(source, destination)?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;

        for entry in
            fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
        {
            let entry = entry.with_context(|| {
                format!("failed to inspect an entry under {}", source.display())
            })?;
            copy_path_with_symlink_behavior(
                &entry.path(),
                &destination.join(entry.file_name()),
                follow_symlinks,
            )?;
        }

        return Ok(());
    }

    if metadata.is_file() {
        create_parent_dir(destination)?;

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

fn copy_path_following_symlinks(
    source: &Path,
    destination: &Path,
    preserved_symlink_paths: &HashSet<PathBuf>,
    active_directories: &mut HashSet<PathBuf>,
) -> Result<()> {
    if preserved_symlink_paths.contains(source)
        && fs::symlink_metadata(source)
            .with_context(|| format!("failed to inspect {}", source.display()))?
            .file_type()
            .is_symlink()
    {
        copy_symlink(source, destination)?;
        return Ok(());
    }

    let metadata = inspect_path(source, true)?;

    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        return copy_directory_contents_following_symlinks(
            source,
            destination,
            preserved_symlink_paths,
            active_directories,
        );
    }

    if metadata.is_file() {
        create_parent_dir(destination)?;
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

fn hardlink_path(source: &Path, destination: &Path) -> Result<()> {
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
            hardlink_path(&entry.path(), &destination.join(entry.file_name()))?;
        }

        return Ok(());
    }

    if metadata.is_file() {
        create_parent_dir(destination)?;
        hard_link_or_copy(source, destination)?;
        return Ok(());
    }

    bail!(
        "unsupported file type while hard-linking playground contents: {}",
        source.display()
    );
}

fn hard_link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            fs::copy(source, destination).with_context(|| {
                format!(
                    "failed to copy {} to {} after cross-device hard-link failure",
                    source.display(),
                    destination.display()
                )
            })?;
            Ok(())
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to hard link {} to {}",
                source.display(),
                destination.display()
            )
        }),
    }
}

fn symlink_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = inspect_path(source, true)?;

    create_parent_dir(destination)?;
    create_symlink(source, destination, metadata.is_dir()).with_context(|| {
        format!(
            "failed to symlink {} to {}",
            source.display(),
            destination.display()
        )
    })?;

    Ok(())
}

fn inspect_path(path: &Path, follow_symlinks: bool) -> Result<fs::Metadata> {
    let metadata = if follow_symlinks {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };

    metadata.with_context(|| format!("failed to inspect {}", path.display()))
}

fn create_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
fn create_symlink(source: &Path, destination: &Path, _is_dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(windows)]
fn create_symlink(source: &Path, destination: &Path, is_dir: bool) -> io::Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(source, destination)
    } else {
        std::os::windows::fs::symlink_file(source, destination)
    }
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
    use std::{
        collections::{BTreeMap, HashSet},
        fs,
        path::{Path, PathBuf},
    };

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::config::{
        AppConfig, ConfigPaths, CreateMode, PlaygroundConfig, PlaygroundDefinition,
        ResolvedAgentConfig,
    };
    use crate::utils::symlink::{copy_symlink, parse_directory_mount};

    use super::{
        DirectoryMount, exit_code_from_status, materialize_playground_contents,
        prompt_to_save_playground_snapshot, run_default_playground, run_default_playground_in_dir,
        run_playground, save_playground_snapshot, should_prompt_to_save_playground_snapshot,
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
            "powershell -NoProfile -Command \"[System.IO.File]::WriteAllText('env.txt', $env:{var_name})\" && if exist .env exit /b 1 && if exist apg.toml exit /b 1"
        )
    }

    #[cfg(unix)]
    fn command_recording_mount(path: &str) -> String {
        format!("cat '{path}' > mounted.txt")
    }

    #[cfg(windows)]
    fn command_recording_mount(path: &str) -> String {
        format!(
            "powershell -NoProfile -Command \"Get-Content -Raw '{path}' | Set-Content -NoNewline mounted.txt\""
        )
    }

    #[cfg(unix)]
    fn command_copy_file(from: &str, to: &str) -> String {
        format!("cat '{from}' > '{to}'")
    }

    #[cfg(windows)]
    fn command_copy_file(from: &str, to: &str) -> String {
        format!(
            "powershell -NoProfile -Command \"Get-Content -Raw '{from}' | Set-Content -NoNewline '{to}'\""
        )
    }

    #[cfg(unix)]
    fn command_copy_file_and_fail(from: &str, to: &str, exit_code: i32) -> String {
        format!("cat '{from}' > '{to}'; exit {exit_code}")
    }

    #[cfg(windows)]
    fn command_copy_file_and_fail(from: &str, to: &str, exit_code: i32) -> String {
        format!(
            "powershell -NoProfile -Command \"Get-Content -Raw '{from}' | Set-Content -NoNewline '{to}'\" && exit /b {exit_code}"
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

    fn make_playground(
        source_dir: &Path,
        playground_id: &str,
        default_agent: Option<&str>,
        load_env: Option<bool>,
    ) -> Result<PlaygroundDefinition> {
        let config_file = source_dir.join("apg.toml");
        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(source_dir.join("notes.txt"), "hello")?;

        Ok(PlaygroundDefinition {
            id: playground_id.to_string(),
            description: "demo".to_string(),
            directory: source_dir.to_path_buf(),
            config_file,
            playground: PlaygroundConfig {
                default_agent: default_agent.map(str::to_string),
                load_env,
                create_mode: None,
            },
        })
    }

    fn make_config(
        source_dir: &Path,
        save_root: &Path,
        playground_id: &str,
        default_agent: Option<&str>,
        playground_default_agent: Option<&str>,
        playground_load_env: Option<bool>,
        agents: &[(&str, String)],
    ) -> Result<AppConfig> {
        let playground = make_playground(
            source_dir,
            playground_id,
            playground_default_agent,
            playground_load_env,
        )?;
        let agents = agents
            .iter()
            .map(|(id, command)| {
                (
                    (*id).to_string(),
                    ResolvedAgentConfig {
                        cmd: command.clone(),
                        config_dir: PathBuf::from(format!(".{id}")),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut playgrounds = BTreeMap::new();
        playgrounds.insert(playground_id.to_string(), playground);

        Ok(AppConfig {
            paths: ConfigPaths::from_root_dir(source_dir.join("config-root")),
            agents,
            default_playground: None,
            saved_playgrounds_dir: save_root.to_path_buf(),
            playground_defaults: PlaygroundConfig {
                default_agent: default_agent.map(str::to_string),
                load_env: Some(false),
                create_mode: None,
            },
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
            directory: source_dir.path().to_path_buf(),
            config_file: config_file.clone(),
            playground: PlaygroundConfig::default(),
        };

        materialize_playground_contents(
            &playground,
            false,
            CreateMode::Copy,
            destination_dir.path(),
        )?;

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
            directory: source_dir.path().to_path_buf(),
            config_file,
            playground: PlaygroundConfig::default(),
        };

        materialize_playground_contents(
            &playground,
            true,
            CreateMode::Copy,
            destination_dir.path(),
        )?;

        assert!(!destination_dir.path().join(".env").exists());
        assert_eq!(
            fs::read_to_string(destination_dir.path().join("notes.txt"))?,
            "hello"
        );

        Ok(())
    }

    #[test]
    fn symlinks_playground_contents_when_requested() -> Result<()> {
        let source_dir = tempdir()?;
        let destination_dir = tempdir()?;
        let config_file = source_dir.path().join("apg.toml");
        let note_file = source_dir.path().join("notes.txt");
        let nested_dir = source_dir.path().join("nested");

        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(&note_file, "hello")?;
        fs::create_dir_all(&nested_dir)?;
        fs::write(nested_dir.join("task.md"), "nested")?;

        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "demo".to_string(),
            directory: source_dir.path().to_path_buf(),
            config_file,
            playground: PlaygroundConfig::default(),
        };

        materialize_playground_contents(
            &playground,
            false,
            CreateMode::Symlink,
            destination_dir.path(),
        )?;

        assert!(
            fs::symlink_metadata(destination_dir.path().join("notes.txt"))?
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(destination_dir.path().join("nested"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(destination_dir.path().join("nested").join("task.md"))?,
            "nested"
        );
        assert!(!destination_dir.path().join("apg.toml").exists());

        Ok(())
    }

    #[test]
    fn hardlinks_playground_files_when_requested() -> Result<()> {
        let source_dir = tempdir()?;
        let destination_dir = tempdir()?;
        let config_file = source_dir.path().join("apg.toml");
        let note_file = source_dir.path().join("notes.txt");
        let nested_dir = source_dir.path().join("nested");
        let nested_file = nested_dir.join("task.md");

        fs::write(&config_file, "description = 'ignored'")?;
        fs::write(&note_file, "hello")?;
        fs::create_dir_all(&nested_dir)?;
        fs::write(&nested_file, "nested")?;

        let playground = PlaygroundDefinition {
            id: "demo".to_string(),
            description: "demo".to_string(),
            directory: source_dir.path().to_path_buf(),
            config_file,
            playground: PlaygroundConfig::default(),
        };

        materialize_playground_contents(
            &playground,
            false,
            CreateMode::Hardlink,
            destination_dir.path(),
        )?;

        let linked_note = destination_dir.path().join("notes.txt");
        let linked_nested = destination_dir.path().join("nested").join("task.md");
        assert!(linked_note.is_file());
        assert!(linked_nested.is_file());
        assert!(!fs::symlink_metadata(&linked_note)?.file_type().is_symlink());
        assert!(
            !fs::symlink_metadata(destination_dir.path().join("nested"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(linked_nested)?, "nested");

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
    fn prompts_only_for_normal_exit_without_explicit_save_flag() {
        assert!(should_prompt_to_save_playground_snapshot(true, false, true));
        assert!(!should_prompt_to_save_playground_snapshot(true, true, true));
        assert!(!should_prompt_to_save_playground_snapshot(
            false, false, true
        ));
        assert!(!should_prompt_to_save_playground_snapshot(
            true, false, false
        ));
    }

    #[test]
    fn prompt_accepts_yes_and_rejects_default_enter() -> Result<()> {
        let mut output = Vec::new();
        let should_save =
            prompt_to_save_playground_snapshot(std::io::Cursor::new("y\n"), &mut output)?;
        assert!(should_save);
        assert_eq!(
            String::from_utf8(output).expect("prompt output"),
            "Keep temporary playground copy? [y/N] "
        );

        let mut output = Vec::new();
        let should_save =
            prompt_to_save_playground_snapshot(std::io::Cursor::new("\n"), &mut output)?;
        assert!(!should_save);

        Ok(())
    }

    #[test]
    fn saves_temporary_playground_snapshot() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let nested_dir = source_dir.path().join("nested");

        fs::create_dir_all(&nested_dir)?;
        fs::write(source_dir.path().join("notes.txt"), "hello")?;
        fs::write(nested_dir.join("task.md"), "nested")?;

        let saved_path =
            save_playground_snapshot(source_dir.path(), save_root.path(), "demo", HashSet::new())?;

        assert!(saved_path.starts_with(save_root.path()));
        assert_eq!(fs::read_to_string(saved_path.join("notes.txt"))?, "hello");
        assert_eq!(
            fs::read_to_string(saved_path.join("nested").join("task.md"))?,
            "nested"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_save_snapshot_when_symlink_cycle_is_detected() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let loop_dir = source_dir.path().join("loop");

        std::os::unix::fs::symlink(source_dir.path(), &loop_dir)?;

        let error =
            save_playground_snapshot(source_dir.path(), save_root.path(), "demo", HashSet::new())
                .expect_err("symlink cycle should fail");

        assert!(
            error
                .to_string()
                .contains("refusing to save playground snapshot")
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
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("default"))],
        )?;

        let error = run_playground(&config, "missing", None, false, &[], None)
            .expect_err("unknown playground");

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
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("default"))],
        )?;

        let error = run_playground(&config, "demo", Some("missing"), false, &[], None)
            .expect_err("unknown agent");

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
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("default"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, true, &[], None)?;
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
    fn runs_empty_default_playground_with_default_agent() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("default"))],
        )?;

        let exit_code = run_default_playground(&config, None, true, &[])?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "default"
        );
        assert!(!snapshot.join("notes.txt").exists());
        assert!(
            snapshot
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("__default__-"))
        );
        Ok(())
    }

    #[test]
    fn selected_agent_overrides_default_in_empty_default_playground() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[
                ("claude", command_writing_marker("default")),
                ("codex", command_writing_marker("selected")),
            ],
        )?;

        let exit_code = run_default_playground(&config, Some("codex"), true, &[])?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "selected"
        );
        Ok(())
    }

    #[test]
    fn uses_playground_default_agent_before_root_default() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            Some("codex"),
            None,
            &[
                ("claude", command_writing_marker("root-default")),
                ("codex", command_writing_marker("playground-default")),
            ],
        )?;

        let exit_code = run_playground(&config, "demo", None, true, &[], None)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "playground-default"
        );
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
            Some("claude"),
            Some("opencode"),
            None,
            &[
                ("claude", command_writing_marker("default")),
                ("opencode", command_writing_marker("playground-default")),
                ("codex", command_writing_marker("selected")),
            ],
        )?;

        let exit_code = run_playground(&config, "demo", Some("codex"), true, &[], None)?;
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
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("default"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, false, &[], None)?;

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
            Some("claude"),
            None,
            None,
            &[("claude", failing_command())],
        )?;

        let exit_code = run_playground(&config, "demo", None, true, &[], None)?;

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
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            Some(true),
            &[("claude", command_recording_env("PLAYGROUND_SECRET"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, true, &[], None)?;
        let snapshot = single_saved_snapshot(save_root.path())?;
        assert_eq!(exit_code, 0);
        assert_eq!(fs::read_to_string(snapshot.join("env.txt"))?, "token-123");
        assert!(!snapshot.join(".env").exists());
        Ok(())
    }

    #[test]
    fn parses_directory_mount_with_default_destination_from_source_name() -> Result<()> {
        let temp = tempdir()?;
        let source = temp.path().join("outside");
        fs::create_dir_all(&source)?;

        let mount = parse_directory_mount(
            source
                .to_str()
                .expect("temporary directory path should be valid UTF-8"),
        )?;

        assert_eq!(
            mount,
            DirectoryMount {
                source: fs::canonicalize(&source)?,
                destination: PathBuf::from("outside"),
            }
        );
        Ok(())
    }

    #[test]
    fn parses_directory_mount_with_explicit_relative_destination() -> Result<()> {
        let temp = tempdir()?;
        let source = temp.path().join("outside");
        fs::create_dir_all(&source)?;

        let mount = parse_directory_mount(&format!("{}:tools/shared", source.display()))?;

        assert_eq!(
            mount,
            DirectoryMount {
                source: fs::canonicalize(&source)?,
                destination: PathBuf::from("tools/shared"),
            }
        );
        Ok(())
    }

    #[test]
    fn rejects_absolute_directory_mount_destination() -> Result<()> {
        let temp = tempdir()?;
        let source = temp.path().join("outside");
        fs::create_dir_all(&source)?;

        let error = parse_directory_mount(&format!("{}:/absolute", source.display()))
            .expect_err("absolute destination should be rejected");

        assert!(error.to_string().contains("must be a relative path"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_symlink_preserves_relative_link_target() -> Result<()> {
        let source_dir = tempdir()?;
        let destination_dir = tempdir()?;
        let nested_dir = source_dir.path().join("nested");
        let target_dir = source_dir.path().join("target");
        let source_link = nested_dir.join("shared");
        let destination_link = destination_dir.path().join("nested").join("shared");

        fs::create_dir_all(&nested_dir)?;
        fs::create_dir_all(&target_dir)?;
        std::os::unix::fs::symlink("../target", &source_link)?;

        copy_symlink(&source_link, &destination_link)?;

        assert_eq!(
            fs::read_link(&destination_link)?,
            PathBuf::from("../target")
        );
        Ok(())
    }

    #[test]
    fn mounts_external_directory_into_playground_and_preserves_symlink_in_snapshot() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let external_dir = tempdir()?;
        fs::write(external_dir.path().join("shared.txt"), "from-outside")?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_recording_mount("tools/shared/shared.txt"))],
        )?;
        let mounts = vec![DirectoryMount {
            source: fs::canonicalize(external_dir.path())?,
            destination: PathBuf::from("tools/shared"),
        }];

        let exit_code = run_playground(&config, "demo", None, true, &mounts, None)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("mounted.txt"))?,
            "from-outside"
        );
        let mounted_path = snapshot.join("tools").join("shared");
        let metadata = fs::symlink_metadata(&mounted_path)?;
        assert!(metadata.file_type().is_symlink());
        assert_eq!(
            fs::read_link(&mounted_path)?,
            fs::canonicalize(external_dir.path())?
        );

        Ok(())
    }

    #[test]
    fn mounts_external_directory_into_empty_default_playground() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let external_dir = tempdir()?;
        fs::write(external_dir.path().join("shared.txt"), "from-outside")?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_recording_mount("shared/shared.txt"))],
        )?;
        let mounts = vec![DirectoryMount {
            source: fs::canonicalize(external_dir.path())?,
            destination: PathBuf::from("shared"),
        }];

        let exit_code = run_default_playground(&config, None, true, &mounts)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("mounted.txt"))?,
            "from-outside"
        );
        assert!(
            fs::symlink_metadata(snapshot.join("shared"))?
                .file_type()
                .is_symlink()
        );

        Ok(())
    }

    #[test]
    fn playground_create_mode_overrides_root_default_during_run() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let mut config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[(
                "claude",
                command_writing_marker("playground-create-mode-override"),
            )],
        )?;
        config.playground_defaults.create_mode = Some(CreateMode::Copy);
        config
            .playgrounds
            .get_mut("demo")
            .expect("demo playground")
            .playground
            .create_mode = Some(CreateMode::Symlink);

        let exit_code = run_playground(&config, "demo", None, true, &[], None)?;
        let snapshot = single_saved_snapshot(save_root.path())?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(snapshot.join("agent.txt"))?.trim(),
            "playground-create-mode-override"
        );
        assert_eq!(fs::read_to_string(snapshot.join("notes.txt"))?, "hello");
        assert!(
            !fs::symlink_metadata(snapshot.join("notes.txt"))?
                .file_type()
                .is_symlink()
        );

        Ok(())
    }

    #[test]
    fn run_playground_in_path_injects_links_and_cleans_up_after_exit() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let in_path = tempdir()?;
        let nested = source_dir.path().join("nested");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("task.md"), "nested-task")?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_copy_file("notes.txt", "captured.txt"))],
        )?;

        let exit_code = run_playground(&config, "demo", None, true, &[], Some(in_path.path()))?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(in_path.path().join("captured.txt"))?,
            "hello"
        );
        assert!(!in_path.path().join("notes.txt").exists());
        assert!(!in_path.path().join("nested").exists());
        assert_eq!(fs::read_dir(save_root.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn run_playground_in_path_merges_directory_entries_without_overwriting_conflicts() -> Result<()>
    {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let in_path = tempdir()?;
        let source_shared = source_dir.path().join("shared");
        let target_shared = in_path.path().join("shared");
        fs::create_dir_all(&source_shared)?;
        fs::create_dir_all(&target_shared)?;
        fs::write(source_shared.join("from-playground.txt"), "playground")?;
        fs::write(source_shared.join("collision.txt"), "playground-collision")?;
        fs::write(target_shared.join("collision.txt"), "user-content")?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[(
                "claude",
                format!(
                    "{} && {}",
                    command_copy_file("shared/from-playground.txt", "linked.txt"),
                    command_copy_file("shared/collision.txt", "collision_seen.txt")
                ),
            )],
        )?;

        let exit_code = run_playground(&config, "demo", None, false, &[], Some(in_path.path()))?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(in_path.path().join("linked.txt"))?,
            "playground"
        );
        assert_eq!(
            fs::read_to_string(in_path.path().join("collision_seen.txt"))?,
            "user-content"
        );
        assert_eq!(
            fs::read_to_string(target_shared.join("collision.txt"))?,
            "user-content"
        );
        assert!(!target_shared.join("from-playground.txt").exists());
        Ok(())
    }

    #[test]
    fn run_default_playground_in_path_mounts_and_cleans_up() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let in_path = tempdir()?;
        let external_dir = tempdir()?;
        fs::write(external_dir.path().join("shared.txt"), "from-outside")?;
        let mounts = vec![DirectoryMount {
            source: fs::canonicalize(external_dir.path())?,
            destination: PathBuf::from("shared"),
        }];

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[(
                "claude",
                command_copy_file("shared/shared.txt", "mounted.txt"),
            )],
        )?;

        let exit_code =
            run_default_playground_in_dir(&config, None, in_path.path(), false, &mounts)?;

        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(in_path.path().join("mounted.txt"))?,
            "from-outside"
        );
        assert!(!in_path.path().join("shared").exists());
        Ok(())
    }

    #[test]
    fn run_playground_in_path_cleans_up_links_on_failing_exit() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let in_path = tempdir()?;
        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[(
                "claude",
                command_copy_file_and_fail("notes.txt", "copied-before-fail.txt", 7),
            )],
        )?;

        let exit_code = run_playground(&config, "demo", None, false, &[], Some(in_path.path()))?;

        assert_eq!(exit_code, 7);
        assert_eq!(
            fs::read_to_string(in_path.path().join("copied-before-fail.txt"))?,
            "hello"
        );
        assert!(!in_path.path().join("notes.txt").exists());
        Ok(())
    }

    #[test]
    fn in_path_is_created_if_missing_and_errors_when_not_directory() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let create_target = source_dir.path().join("new-run-dir");
        let file_target = source_dir.path().join("not-a-directory");
        fs::write(&file_target, "file")?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("ok"))],
        )?;

        let exit_code = run_default_playground_in_dir(&config, None, &create_target, false, &[])?;
        assert_eq!(exit_code, 0);
        assert!(create_target.is_dir());

        let error = run_default_playground_in_dir(&config, None, &file_target, false, &[])
            .expect_err("non-directory in_path should fail");
        assert!(error.to_string().contains("exists but is not a directory"));

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn in_path_accepts_symlink_to_directory() -> Result<()> {
        let source_dir = tempdir()?;
        let save_root = tempdir()?;
        let target_dir = tempdir()?;
        let in_path_symlink = source_dir.path().join("in-path-link");
        std::os::unix::fs::symlink(target_dir.path(), &in_path_symlink)?;

        let config = make_config(
            source_dir.path(),
            save_root.path(),
            "demo",
            Some("claude"),
            None,
            None,
            &[("claude", command_writing_marker("ok"))],
        )?;

        let exit_code = run_default_playground_in_dir(&config, None, &in_path_symlink, false, &[])?;
        assert_eq!(exit_code, 0);
        assert_eq!(
            fs::read_to_string(target_dir.path().join("agent.txt"))?,
            "ok"
        );

        Ok(())
    }
}

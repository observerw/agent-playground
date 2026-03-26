use std::{
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
/// A directory mounted into the temporary playground as a symbolic link.
pub struct DirectoryMount {
    /// Absolute source directory on the host filesystem.
    pub source: PathBuf,
    /// Relative destination path inside the temporary playground.
    pub destination: PathBuf,
}

impl FromStr for DirectoryMount {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        parse_directory_mount(raw).map_err(|error| error.to_string())
    }
}

pub(crate) fn parse_directory_mount(raw: &str) -> Result<DirectoryMount> {
    if let Ok(source) = resolve_directory_mount_source(raw) {
        return Ok(DirectoryMount {
            destination: default_directory_mount_destination(&source)?,
            source,
        });
    }

    let (source_raw, destination_raw) = raw
        .rsplit_once(':')
        .context("mount spec must be SOURCE or SOURCE:RELATIVE_DESTINATION")?;
    let source = resolve_directory_mount_source(source_raw)?;
    let destination = parse_directory_mount_destination(destination_raw)?;

    Ok(DirectoryMount {
        source,
        destination,
    })
}

pub(crate) fn apply_directory_mounts(working_dir: &Path, mounts: &[DirectoryMount]) -> Result<()> {
    for mount in mounts {
        let destination = working_dir.join(&mount.destination);
        ensure_destination_absent(&destination)?;

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        create_symlink(&mount.source, &destination, true).with_context(|| {
            format!(
                "failed to mount {} at {}",
                mount.source.display(),
                destination.display()
            )
        })?;
    }

    Ok(())
}

pub(crate) fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let link_target = fs::read_link(source)
        .with_context(|| format!("failed to read symlink {}", source.display()))?;

    #[cfg(unix)]
    let is_dir_target = false;

    #[cfg(windows)]
    let is_dir_target = fs::metadata(source)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);

    create_symlink(&link_target, destination, is_dir_target).with_context(|| {
        format!(
            "failed to recreate symlink {} -> {}",
            destination.display(),
            link_target.display()
        )
    })?;

    Ok(())
}

fn resolve_directory_mount_source(raw: &str) -> Result<PathBuf> {
    let source = fs::canonicalize(raw)
        .with_context(|| format!("failed to resolve mount source directory '{}'", raw))?;
    let metadata = fs::metadata(&source)
        .with_context(|| format!("failed to inspect mount source {}", source.display()))?;

    if !metadata.is_dir() {
        bail!("mount source '{}' is not a directory", raw);
    }

    Ok(source)
}

fn default_directory_mount_destination(source: &Path) -> Result<PathBuf> {
    source
        .file_name()
        .map(PathBuf::from)
        .context("mount source must have a directory name or an explicit destination")
}

fn parse_directory_mount_destination(raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        bail!("mount destination cannot be empty");
    }

    let destination = PathBuf::from(raw);
    if destination.is_absolute() {
        bail!(
            "mount destination '{}' must be a relative path inside the playground",
            raw
        );
    }

    for component in destination.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            bail!(
                "mount destination '{}' must only contain normal path segments",
                raw
            );
        }
    }

    Ok(destination)
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

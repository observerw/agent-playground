//! JSON Schema site generation utilities.
//!
//! This module materializes schema files for the public configuration models
//! and writes a tiny static index page that can be hosted as documentation.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{PlaygroundConfigFile, RootConfigFile};

const ROOT_SCHEMA_FILE_NAME: &str = "root-config.schema.json";
const PLAYGROUND_SCHEMA_FILE_NAME: &str = "playground-config.schema.json";

/// Generates JSON Schema artifacts and an index page into `output_dir`.
///
/// The command writes:
///
/// - `schemas/root-config.schema.json`
/// - `schemas/playground-config.schema.json`
/// - `index.html`
///
/// Existing files at those paths are overwritten.
///
/// # Errors
///
/// Returns an error when output directories cannot be created, schema values
/// cannot be serialized, or files cannot be written.
pub fn write_schema_site(output_dir: &Path) -> Result<()> {
    let schemas_dir = output_dir.join("schemas");
    fs::create_dir_all(&schemas_dir)
        .with_context(|| format!("failed to create {}", schemas_dir.display()))?;

    write_json_file(
        &schemas_dir.join(ROOT_SCHEMA_FILE_NAME),
        &RootConfigFile::json_schema(),
    )?;
    write_json_file(
        &schemas_dir.join(PLAYGROUND_SCHEMA_FILE_NAME),
        &PlaygroundConfigFile::json_schema(),
    )?;
    fs::write(output_dir.join("index.html"), render_schema_index()).with_context(|| {
        format!(
            "failed to write {}",
            output_dir.join("index.html").display()
        )
    })?;

    Ok(())
}

/// Returns the default output location used for generated schema artifacts.
///
/// The returned path is relative to the current working directory:
/// `target/schema-site`.
pub fn default_schema_site_dir() -> PathBuf {
    PathBuf::from("target").join("schema-site")
}

fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let content =
        serde_json::to_string_pretty(value).context("failed to serialize schema to JSON")?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn render_schema_index() -> &'static str {
    r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>agent-playground JSON Schemas</title>
    <style>
      :root {
        color-scheme: light dark;
        font-family: ui-sans-serif, system-ui, sans-serif;
      }
      body {
        margin: 0 auto;
        max-width: 48rem;
        padding: 3rem 1.25rem;
        line-height: 1.6;
      }
      code {
        font-family: ui-monospace, SFMono-Regular, monospace;
      }
    </style>
  </head>
  <body>
    <h1>agent-playground JSON Schemas</h1>
    <p>Generated from the Rust config file models in this repository.</p>
    <ul>
      <li><a href="./schemas/root-config.schema.json"><code>root-config.schema.json</code></a></li>
      <li><a href="./schemas/playground-config.schema.json"><code>playground-config.schema.json</code></a></li>
    </ul>
  </body>
</html>
"#
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{PLAYGROUND_SCHEMA_FILE_NAME, ROOT_SCHEMA_FILE_NAME, write_schema_site};

    #[test]
    fn writes_schema_site_artifacts() -> Result<()> {
        let output_dir = tempdir()?;

        write_schema_site(output_dir.path())?;

        let root_schema_path = output_dir
            .path()
            .join("schemas")
            .join(ROOT_SCHEMA_FILE_NAME);
        let playground_schema_path = output_dir
            .path()
            .join("schemas")
            .join(PLAYGROUND_SCHEMA_FILE_NAME);
        let index_path = output_dir.path().join("index.html");

        assert!(root_schema_path.is_file());
        assert!(playground_schema_path.is_file());
        assert!(index_path.is_file());

        let root_schema: Value = serde_json::from_str(&std::fs::read_to_string(root_schema_path)?)?;
        let playground_schema: Value =
            serde_json::from_str(&std::fs::read_to_string(playground_schema_path)?)?;
        let index_html = std::fs::read_to_string(index_path)?;

        assert_eq!(root_schema["type"], Value::String("object".to_string()));
        assert_eq!(
            playground_schema["type"],
            Value::String("object".to_string())
        );
        assert!(index_html.contains("root-config.schema.json"));
        assert!(index_html.contains("playground-config.schema.json"));

        Ok(())
    }
}

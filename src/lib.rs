#![warn(missing_docs)]
//! `agent-playground` library crate.
//!
//! This crate powers the `apg` CLI and exposes reusable building blocks for:
//! configuration loading/initialization, playground listing, playground
//! execution, and JSON schema generation.
//!
//! # Module Guide
//!
//! - [`config`]: file-backed configuration models and initialization helpers.
//! - [`info`]: terminal-friendly detailed output for one configured playground.
//! - [`listing`]: terminal-friendly listing output for known playgrounds.
//! - [`runner`]: runtime orchestration for launching agents in temporary copies.
//! - [`schema`]: JSON Schema export utilities for configuration file models.
//!
//! Most applications use [`config::AppConfig`] as the entry point and then call
//! higher-level helpers such as [`listing::list_playgrounds`] or
//! [`runner::run_playground`].

/// Configuration models and helpers for loading and initializing playgrounds.
pub mod config;
/// Detailed information helpers for rendering one playground in CLI output.
pub mod info;
/// Listing helpers for rendering configured playgrounds in CLI output.
pub mod listing;
/// Runtime orchestration for launching agents in isolated temporary directories.
pub mod runner;
/// JSON Schema and static site generation for configuration file formats.
pub mod schema;

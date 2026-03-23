//! Configuration parsing and management.
//!
//! Reads and writes `buildwatch.config.json`. Supports hot-reload.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = "buildwatch.config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Schema version for forward compatibility
    #[serde(default = "default_version")]
    pub version: u32,

    /// Debounce period before triggering a build (ms)
    #[serde(default = "default_settling_delay")]
    pub settling_delay_ms: u64,

    /// Maximum build duration before timeout (seconds)
    #[serde(default = "default_build_timeout")]
    pub build_timeout_seconds: u64,

    /// Notification settings
    #[serde(default)]
    pub notifications: NotificationConfig,

    /// Build targets
    pub targets: Vec<TargetConfig>,

    /// Global ignore patterns (in addition to .gitignore)
    #[serde(default = "default_global_excludes")]
    pub global_excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub on_success: bool,
    #[serde(default = "default_true")]
    pub on_failure: bool,
    #[serde(default = "default_true")]
    pub sound: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_success: true,
            on_failure: true,
            sound: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConfig {
    /// Human-readable target name
    pub name: String,

    /// Shell command to execute for building
    pub build_command: String,

    /// Path to the build output artifact (relative to project root)
    pub output_path: Option<String>,

    /// Working directory for the build command (relative to project root)
    #[serde(default = "default_working_dir")]
    pub working_directory: String,

    /// File extensions to watch
    #[serde(default)]
    pub watch_extensions: Vec<String>,

    /// Directories to watch (relative to project root)
    #[serde(default)]
    pub watch_paths: Vec<String>,

    /// Directories to exclude from watching
    #[serde(default)]
    pub exclude_paths: Vec<String>,

    /// Environment variables for the build command
    #[serde(default)]
    pub environment: HashMap<String, String>,

    /// Priority for build ordering (higher = built first)
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Whether this target is active
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Command to run after successful build
    pub post_build: Option<String>,

    /// Auto-restart the running binary after rebuild
    #[serde(default)]
    pub auto_restart: bool,
}

// --- Default value functions ---

fn default_version() -> u32 { 1 }
fn default_settling_delay() -> u64 { 200 }
fn default_build_timeout() -> u64 { 300 }
fn default_true() -> bool { true }
fn default_priority() -> i32 { 5 }
fn default_working_dir() -> String { ".".to_string() }

fn default_global_excludes() -> Vec<String> {
    vec![
        ".git/".into(), "node_modules/".into(), "__pycache__/".into(),
        "target/".into(), "bin/".into(), ".next/".into(), "dist/".into(),
    ]
}

// --- Public API ---

/// Load configuration from `buildwatch.config.json` in the given project root.
pub fn load_config(project_root: &Path) -> Result<Config> {
    let config_path = project_root.join(CONFIG_FILENAME);
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let config: Config = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;
    Ok(config)
}

/// Write configuration to `buildwatch.config.json` in the given project root.
pub fn write_config(project_root: &Path, config: &Config) -> Result<()> {
    let config_path = project_root.join(CONFIG_FILENAME);
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&config_path, content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;
    tracing::info!("Wrote config to {}", config_path.display());
    Ok(())
}

/// Returns the config file path for a given project root.
pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(CONFIG_FILENAME)
}

//! Shared state management via JSON files on disk.
//!
//! All writes are atomic (tempfile + rename). The state directory is the
//! sole IPC mechanism between the daemon, `wr`, and `buildwatch status`.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maximum heartbeat age before daemon is considered dead (seconds).
const HEARTBEAT_TIMEOUT_SECS: i64 = 15;

// --- State directory layout ---

/// Returns the platform-appropriate base state directory.
fn base_state_dir() -> PathBuf {
    crate::state_dir()
}

/// Returns the state directory for a given project root.
pub fn state_dir_for(project_root: &Path) -> PathBuf {
    base_state_dir().join(crate::project_hash(project_root))
}

/// Ensure the state directory exists.
pub fn ensure_state_dir(project_root: &Path) -> Result<PathBuf> {
    let dir = state_dir_for(project_root);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create state dir: {}", dir.display()))?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalRegistryEntry {
    pub project_root: String,
    pub project_hash: String,
    pub pid: u32,
    pub heartbeat: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRegistry {
    pub projects: Vec<GlobalRegistryEntry>,
    pub updated_at: Option<DateTime<Utc>>,
}

// --- Data structures ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub project_root: String,
    pub project_hash: String,
    pub started_at: DateTime<Utc>,
    pub heartbeat: DateTime<Utc>,
    pub version: String,
    pub watchman_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    pub targets: HashMap<String, TargetState>,
    pub queue: QueueState,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetState {
    /// "ready" | "building" | "failed" | "pending" | "stale"
    pub status: String,
    pub last_build: Option<BuildResult>,
    pub build_count: u64,
    pub failure_count: u64,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub exit_code: i32,
    pub output_path: Option<String>,
    pub trigger_files: Vec<String>,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueState {
    pub pending: Vec<String>,
    pub current: Option<String>,
}

// --- Atomic file write ---

/// Write content to a file atomically (tempfile + rename).
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().context("No parent directory")?;
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), content)?;
    tmp.persist(path)
        .with_context(|| format!("Failed to persist {}", path.display()))?;
    Ok(())
}

// --- Read operations (used by `wr` and `buildwatch status`) ---

/// Read daemon info from the state directory.
pub fn read_daemon_info(state_dir: &Path) -> Result<DaemonInfo> {
    let path = state_dir.join("daemon.json");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let info: DaemonInfo = serde_json::from_str(&content)?;
    Ok(info)
}

/// Check if a daemon is alive based on its heartbeat.
pub fn is_daemon_alive(info: &DaemonInfo) -> bool {
    let age = Utc::now().signed_duration_since(info.heartbeat);
    age.num_seconds() < HEARTBEAT_TIMEOUT_SECS
}

/// Read project state from the state directory.
pub fn read_state(state_dir: &Path) -> Result<ProjectState> {
    let path = state_dir.join("state.json");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let state: ProjectState = serde_json::from_str(&content)?;
    Ok(state)
}

// --- Write operations (used by daemon) ---

/// Write daemon info atomically.
pub fn write_daemon_info(state_dir: &Path, info: &DaemonInfo) -> Result<()> {
    let path = state_dir.join("daemon.json");
    atomic_write(&path, &serde_json::to_string_pretty(info)?)
}

/// Write project state atomically.
pub fn write_state(state_dir: &Path, state: &ProjectState) -> Result<()> {
    let path = state_dir.join("state.json");
    atomic_write(&path, &serde_json::to_string_pretty(state)?)
}

/// Update the daemon heartbeat timestamp.
pub fn update_heartbeat(state_dir: &Path) -> Result<()> {
    let mut info = read_daemon_info(state_dir)?;
    info.heartbeat = Utc::now();
    write_daemon_info(state_dir, &info)
}

// --- Fuzzy target matching ---

/// Resolve a target name with fuzzy/substring matching.
/// Returns the exact target name if found, or matches on substring.
/// Errors on ambiguous matches.
pub fn fuzzy_match_target(query: &str, state: &ProjectState) -> Result<String> {
    let targets: Vec<&String> = state.targets.keys().collect();

    // Exact match first
    if state.targets.contains_key(query) {
        return Ok(query.to_string());
    }

    // Substring match
    let matches: Vec<&&String> = targets
        .iter()
        .filter(|t| t.to_lowercase().contains(&query.to_lowercase()))
        .collect();

    match matches.len() {
        0 => bail!(
            "No target matching '{}'. Available: {}",
            query,
            targets
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1 => Ok(matches[0].to_string()),
        _ => bail!(
            "Ambiguous target '{}'. Matches: {}",
            query,
            matches
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// --- Status and cleanup ---

/// Print status of all active daemons.
pub fn print_status(verbose: bool, json_output: bool) -> Result<()> {
    let base = base_state_dir();
    if !base.exists() {
        if json_output {
            println!("[]");
        } else {
            println!("No active BuildWatch daemons.");
        }
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let state_dir = entry.path();
        if let Ok(info) = read_daemon_info(&state_dir) {
            let alive = is_daemon_alive(&info);
            let state = read_state(&state_dir).ok();
            entries.push((info, alive, state));
        }
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No active BuildWatch daemons.");
        return Ok(());
    }

    for (info, alive, state) in &entries {
        let status_icon = if *alive { "👀" } else { "💀" };
        println!("{} {} (pid {})", status_icon, info.project_root, info.pid);
        if let Some(state) = state {
            for (name, ts) in &state.targets {
                let icon = match ts.status.as_str() {
                    "ready" => "✓",
                    "building" => "⟳",
                    "failed" => "✗",
                    "stale" => "?",
                    _ => "-",
                };
                print!("  {} {} [{}]", icon, name, ts.status);
                if verbose {
                    if let Some(ref lb) = ts.last_build {
                        print!(" ({:.1}s)", lb.duration_ms as f64 / 1000.0);
                    }
                    print!(" builds:{} fails:{}", ts.build_count, ts.failure_count);
                }
                println!();
            }
        }
    }
    Ok(())
}

/// Remove all state files for a project.
pub fn clean_state(project_root: &Path) -> Result<()> {
    let dir = state_dir_for(project_root);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("Failed to remove {}", dir.display()))?;
    }
    Ok(())
}

pub fn acquire_lock(state_dir: &Path, pid: u32) -> Result<()> {
    let lock_path = state_dir.join("lock");
    if lock_path.exists() {
        let existing = std::fs::read_to_string(&lock_path).unwrap_or_default();
        if let Ok(existing_pid) = existing.trim().parse::<u32>() {
            if process_alive(existing_pid) {
                bail!("Daemon lock already held by pid {}", existing_pid);
            }
        }
    }
    atomic_write(&lock_path, &format!("{pid}\n"))
}

pub fn release_lock(state_dir: &Path) -> Result<()> {
    let lock_path = state_dir.join("lock");
    if lock_path.exists() {
        std::fs::remove_file(&lock_path)
            .with_context(|| format!("Failed to remove {}", lock_path.display()))?;
    }
    Ok(())
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

fn read_global_registry(base: &Path) -> Result<GlobalRegistry> {
    let path = base.join("global.json");
    if !path.exists() {
        return Ok(GlobalRegistry::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&content)?)
}

fn write_global_registry(base: &Path, registry: &GlobalRegistry) -> Result<()> {
    let path = base.join("global.json");
    atomic_write(&path, &serde_json::to_string_pretty(registry)?)
}

pub fn register_project(project_root: &Path, info: &DaemonInfo) -> Result<()> {
    let base = base_state_dir();
    std::fs::create_dir_all(&base)
        .with_context(|| format!("Failed to create state root {}", base.display()))?;
    let mut registry = read_global_registry(&base)?;
    let root = project_root.to_string_lossy().to_string();
    registry
        .projects
        .retain(|p| p.project_root != root && process_alive(p.pid));
    registry.projects.push(GlobalRegistryEntry {
        project_root: root,
        project_hash: info.project_hash.clone(),
        pid: info.pid,
        heartbeat: info.heartbeat,
    });
    registry.updated_at = Some(Utc::now());
    write_global_registry(&base, &registry)
}

pub fn unregister_project(project_root: &Path) -> Result<()> {
    let base = base_state_dir();
    if !base.exists() {
        return Ok(());
    }
    let mut registry = read_global_registry(&base)?;
    let root = project_root.to_string_lossy().to_string();
    registry
        .projects
        .retain(|p| p.project_root != root && process_alive(p.pid));
    registry.updated_at = Some(Utc::now());
    write_global_registry(&base, &registry)
}

/// Tail the build log for a project/target.
pub fn tail_log(project_root: &Path, _target: Option<&str>) -> Result<()> {
    let dir = state_dir_for(project_root);
    let log_path = dir.join("build.log");
    if !log_path.exists() {
        println!("No build log found.");
        return Ok(());
    }
    let content = std::fs::read_to_string(&log_path)?;
    // Show last 50 lines
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(50);
    for line in &lines[start..] {
        if let Some(target) = _target {
            let prefix = format!("[{}]", target);
            if !line.contains(&prefix) {
                continue;
            }
        }
        println!("{}", line);
    }
    Ok(())
}

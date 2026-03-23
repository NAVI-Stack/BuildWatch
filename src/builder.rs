//! Build execution engine.
//!
//! Spawns build commands as child processes with timeout, output capture,
//! and state coordination via Watchman state assertions.

use crate::config::{Config, TargetConfig};
use crate::queue::PendingBuild;
use crate::state::{self, BuildResult, ProjectState, TargetState};
use crate::watcher::Watcher;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Maximum lines of stderr to capture for error_summary.
const ERROR_SUMMARY_LINES: usize = 5;

fn week_tag(ts: DateTime<Utc>) -> String {
    let iso = ts.iso_week();
    format!("{:04}-W{:02}", iso.year(), iso.week())
}

fn rotate_log_if_needed(state_dir: &Path) -> Result<()> {
    let log_path = state_dir.join("build.log");
    if !log_path.exists() {
        return Ok(());
    }
    let meta = std::fs::metadata(&log_path)?;
    let modified = meta.modified()?;
    let modified_dt: DateTime<Utc> = modified.into();
    if week_tag(modified_dt) == week_tag(Utc::now()) {
        return Ok(());
    }

    let rotated = state_dir.join(format!("build.log.{}", week_tag(modified_dt)));
    if rotated.exists() {
        std::fs::remove_file(&rotated)?;
    }
    std::fs::rename(&log_path, &rotated).with_context(|| {
        format!(
            "Failed rotating log {} -> {}",
            log_path.display(),
            rotated.display()
        )
    })?;
    Ok(())
}

/// Execute a build for a pending build request.
pub async fn execute_build(
    project_root: &Path,
    state_dir: &Path,
    config: &Config,
    watcher: &Watcher,
    pending: &PendingBuild,
) -> Result<BuildResult> {
    let target = config
        .targets
        .iter()
        .find(|t| t.name == pending.target_name)
        .context(format!(
            "Target '{}' not found in config",
            pending.target_name
        ))?;

    let started_at = Utc::now();
    tracing::info!("Building '{}': {}", target.name, target.build_command);

    // Update state to "building"
    update_target_status(state_dir, &target.name, "building")?;

    // Assert Watchman state to defer change events during build
    if let Err(e) = watcher.state_enter().await {
        tracing::warn!("Failed to assert buildwatch.build state: {}", e);
    }

    // Resolve working directory
    let work_dir = project_root.join(&target.working_directory);

    // Build the command (platform-aware shell)
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", &target.build_command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &target.build_command]);
        c
    };

    cmd.current_dir(&work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Inject environment variables
    for (key, val) in &target.environment {
        cmd.env(key, val);
    }

    // Spawn and capture output
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn: {}", target.build_command))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    rotate_log_if_needed(state_dir)?;

    // Stream output to log file
    let log_path = state_dir.join("build.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_file = Arc::new(Mutex::new(log_file));

    let error_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Capture stdout and stderr concurrently.
    let mut log_tasks = Vec::new();
    if let Some(stdout) = stdout {
        let target_name = target.name.clone();
        let log_file = Arc::clone(&log_file);
        log_tasks.push(tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                use std::io::Write;
                if let Ok(mut file) = log_file.lock() {
                    let _ = writeln!(file, "[{}] stdout: {}", target_name, line);
                }
            }
        }));
    }
    if let Some(stderr) = stderr {
        let target_name = target.name.clone();
        let log_file = Arc::clone(&log_file);
        let error_lines = Arc::clone(&error_lines);
        log_tasks.push(tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                use std::io::Write;
                if let Ok(mut file) = log_file.lock() {
                    let _ = writeln!(file, "[{}] stderr: {}", target_name, line);
                }
                if let Ok(mut lines) = error_lines.lock() {
                    if lines.len() < ERROR_SUMMARY_LINES {
                        lines.push(line);
                    }
                }
            }
        }));
    }

    // Wait for completion with timeout
    let timeout_duration = Duration::from_secs(config.build_timeout_seconds);
    let result = timeout(timeout_duration, child.wait()).await;
    for task in log_tasks {
        let _ = task.await;
    }

    let error_lines = error_lines
        .lock()
        .map(|lines| lines.clone())
        .unwrap_or_default();

    // Release Watchman state
    if let Err(e) = watcher.state_leave().await {
        tracing::warn!("Failed to release buildwatch.build state: {}", e);
    }

    let finished_at = Utc::now();
    let duration_ms = (finished_at - started_at).num_milliseconds() as u64;

    let (exit_code, error_summary) = match result {
        Ok(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            let summary = if code != 0 && !error_lines.is_empty() {
                Some(error_lines.join("\n"))
            } else {
                None
            };
            (code, summary)
        }
        Ok(Err(e)) => {
            tracing::error!("Build process error: {}", e);
            (-1, Some(format!("Process error: {}", e)))
        }
        Err(_) => {
            tracing::error!("Build timed out after {}s", config.build_timeout_seconds);
            // Attempt to kill the child process
            child.kill().await.ok();
            (
                -1,
                Some(format!(
                    "Build timed out after {}s",
                    config.build_timeout_seconds
                )),
            )
        }
    };

    let build_result = BuildResult {
        started_at,
        finished_at,
        duration_ms,
        exit_code,
        output_path: target.output_path.clone(),
        trigger_files: pending.trigger_files.clone(),
        error_summary,
    };

    // Update state with result
    let status = if exit_code == 0 { "ready" } else { "failed" };
    update_target_build(state_dir, &target.name, status, &build_result)?;

    if exit_code == 0 {
        tracing::info!(
            "✓ '{}' built ({:.1}s)",
            target.name,
            duration_ms as f64 / 1000.0
        );
    } else {
        tracing::warn!("✗ '{}' failed (exit {})", target.name, exit_code);
    }

    Ok(build_result)
}

/// Update a target's status in state.json.
fn update_target_status(state_dir: &Path, target_name: &str, status: &str) -> Result<()> {
    let mut project_state = state::read_state(state_dir).unwrap_or_else(|_| ProjectState {
        targets: std::collections::HashMap::new(),
        queue: state::QueueState {
            pending: vec![],
            current: None,
        },
        updated_at: Utc::now(),
    });

    let ts = project_state
        .targets
        .entry(target_name.to_string())
        .or_insert_with(|| TargetState {
            status: "pending".into(),
            last_build: None,
            build_count: 0,
            failure_count: 0,
            consecutive_failures: 0,
        });

    ts.status = status.to_string();
    project_state.updated_at = Utc::now();
    state::write_state(state_dir, &project_state)
}

/// Update a target's build result in state.json.
fn update_target_build(
    state_dir: &Path,
    target_name: &str,
    status: &str,
    result: &BuildResult,
) -> Result<()> {
    let mut project_state = state::read_state(state_dir).unwrap_or_else(|_| ProjectState {
        targets: std::collections::HashMap::new(),
        queue: state::QueueState {
            pending: vec![],
            current: None,
        },
        updated_at: Utc::now(),
    });

    let ts = project_state
        .targets
        .entry(target_name.to_string())
        .or_insert_with(|| TargetState {
            status: "pending".into(),
            last_build: None,
            build_count: 0,
            failure_count: 0,
            consecutive_failures: 0,
        });

    ts.status = status.to_string();
    ts.last_build = Some(result.clone());
    ts.build_count += 1;
    if result.exit_code != 0 {
        ts.failure_count += 1;
        ts.consecutive_failures += 1;
    } else {
        ts.consecutive_failures = 0;
    }

    project_state.updated_at = Utc::now();
    state::write_state(state_dir, &project_state)
}

/// Manual build triggered by `buildwatch build [target]`.
pub async fn manual_build(
    project_root: &Path,
    config: &Config,
    target_name: Option<&str>,
) -> Result<()> {
    let state_dir = state::ensure_state_dir(project_root)?;

    let targets: Vec<&TargetConfig> = match target_name {
        Some(name) => {
            let t = config
                .targets
                .iter()
                .find(|t| t.name == name)
                .context(format!("Target '{}' not found", name))?;
            vec![t]
        }
        None => config.targets.iter().filter(|t| t.enabled).collect(),
    };

    // For manual builds without a running daemon, we skip Watchman state assertions
    for target in targets {
        tracing::info!("Manual build: {}", target.name);
        let started_at = Utc::now();
        let work_dir = project_root.join(&target.working_directory);

        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", &target.build_command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &target.build_command]);
            c
        };

        cmd.current_dir(&work_dir);
        for (key, val) in &target.environment {
            cmd.env(key, val);
        }

        let status = cmd
            .status()
            .await
            .context(format!("Failed to run: {}", target.build_command))?;

        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds() as u64;
        let exit_code = status.code().unwrap_or(-1);

        let result = BuildResult {
            started_at,
            finished_at,
            duration_ms,
            exit_code,
            output_path: target.output_path.clone(),
            trigger_files: vec!["manual".into()],
            error_summary: None,
        };

        let status_str = if exit_code == 0 { "ready" } else { "failed" };
        update_target_build(&state_dir, &target.name, status_str, &result)?;

        if exit_code == 0 {
            println!(
                "✓ {} built ({:.1}s)",
                target.name,
                duration_ms as f64 / 1000.0
            );
        } else {
            eprintln!("✗ {} failed (exit {})", target.name, exit_code);
        }
    }

    Ok(())
}

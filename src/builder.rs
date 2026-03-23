//! Build execution engine.
//!
//! Spawns build commands as child processes with timeout, output capture,
//! and state coordination via Watchman state assertions.

use crate::config::{Config, TargetConfig};
use crate::queue::PendingBuild;
use crate::state::{self, BuildResult, ProjectState, TargetState};
use crate::watcher::Watcher;
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Maximum lines of stderr to capture for error_summary.
const ERROR_SUMMARY_LINES: usize = 5;

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
        .context(format!("Target '{}' not found in config", pending.target_name))?;

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
    let mut child = cmd.spawn()
        .with_context(|| format!("Failed to spawn: {}", target.build_command))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Stream output to log file
    let log_path = state_dir.join("build.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let mut error_lines: Vec<String> = Vec::new();

    // Capture stderr for error summary
    if let Some(stderr) = stderr {
        let mut reader = BufReader::new(stderr).lines();
        let mut log_writer = std::io::BufWriter::new(&log_file);
        while let Ok(Some(line)) = reader.next_line().await {
            use std::io::Write;
            writeln!(log_writer, "[{}] stderr: {}", target.name, line)?;
            if error_lines.len() < ERROR_SUMMARY_LINES {
                error_lines.push(line);
            }
        }
    }

    // Wait for completion with timeout
    let timeout_duration = Duration::from_secs(config.build_timeout_seconds);
    let result = timeout(timeout_duration, child.wait()).await;

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
            (-1, Some(format!("Build timed out after {}s", config.build_timeout_seconds)))
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
        tracing::info!("✓ '{}' built ({:.1}s)", target.name, duration_ms as f64 / 1000.0);
    } else {
        tracing::warn!("✗ '{}' failed (exit {})", target.name, exit_code);
    }

    Ok(build_result)
}

/// Update a target's status in state.json.
fn update_target_status(state_dir: &Path, target_name: &str, status: &str) -> Result<()> {
    let mut project_state = state::read_state(state_dir).unwrap_or_else(|_| ProjectState {
        targets: std::collections::HashMap::new(),
        queue: state::QueueState { pending: vec![], current: None },
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
        queue: state::QueueState { pending: vec![], current: None },
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
            let t = config.targets.iter().find(|t| t.name == name)
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

        let status = cmd.status().await
            .context(format!("Failed to run: {}", target.build_command))?;

        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds() as u64;
        let exit_code = status.code().unwrap_or(-1);

        let result = BuildResult {
            started_at, finished_at, duration_ms, exit_code,
            output_path: target.output_path.clone(),
            trigger_files: vec!["manual".into()],
            error_summary: None,
        };

        let status_str = if exit_code == 0 { "ready" } else { "failed" };
        update_target_build(&state_dir, &target.name, status_str, &result)?;

        if exit_code == 0 {
            println!("✓ {} built ({:.1}s)", target.name, duration_ms as f64 / 1000.0);
        } else {
            eprintln!("✗ {} failed (exit {})", target.name, exit_code);
        }
    }

    Ok(())
}

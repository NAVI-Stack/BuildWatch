//! Background daemon lifecycle management.
//!
//! Handles daemonization, heartbeat, graceful shutdown, and the
//! main event loop that connects Watcher → Queue → Builder.

use crate::builder;
use crate::config::Config;
use crate::notifier;
use crate::queue::BuildQueue;
use crate::state::{self, DaemonInfo, ProjectState, QueueState};
use crate::watcher::Watcher;
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

/// Run the daemon in the foreground (blocking).
pub async fn run_foreground(project_root: PathBuf, config: Config) -> Result<()> {
    let state_dir = state::ensure_state_dir(&project_root)?;
    state::acquire_lock(&state_dir, std::process::id())?;
    let run_result: Result<()> = async {

    // Check for existing daemon
    if let Ok(info) = state::read_daemon_info(&state_dir) {
        if state::is_daemon_alive(&info) {
            anyhow::bail!(
                "Daemon already running (pid {}). Stop it first: buildwatch stop",
                info.pid
            );
        }
    }

    // Write initial daemon info
    let daemon_info = DaemonInfo {
        pid: std::process::id(),
        project_root: project_root.to_string_lossy().to_string(),
        project_hash: state_dir.file_name().unwrap().to_string_lossy().to_string(),
        started_at: Utc::now(),
        heartbeat: Utc::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        watchman_version: None,
    };
    state::write_daemon_info(&state_dir, &daemon_info)?;
    state::register_project(&project_root, &daemon_info)?;

    // Write initial state
    let initial_state = ProjectState {
        targets: config
            .targets
            .iter()
            .filter(|t| t.enabled)
            .map(|t| {
                (
                    t.name.clone(),
                    state::TargetState {
                        status: "pending".into(),
                        last_build: None,
                        build_count: 0,
                        failure_count: 0,
                        consecutive_failures: 0,
                    },
                )
            })
            .collect(),
        queue: QueueState {
            pending: vec![],
            current: None,
        },
        updated_at: Utc::now(),
    };
    state::write_state(&state_dir, &initial_state)?;

    tracing::info!(
        "BuildWatch daemon started (pid {}) for {:?}",
        std::process::id(),
        project_root
    );

    // Connect to Watchman
    let mut watcher = Watcher::connect(&project_root).await?;

    // Collect all watch extensions and excludes from enabled targets
    let watch_extensions: Vec<String> = config
        .targets
        .iter()
        .filter(|t| t.enabled)
        .flat_map(|t| t.watch_extensions.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let exclude_dirs: Vec<String> = config
        .global_excludes
        .iter()
        .chain(config.targets.iter().flat_map(|t| t.exclude_paths.iter()))
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Create channel for file change events
    let (tx, mut rx) = mpsc::channel(100);

    // Start subscription
    watcher
        .subscribe(&watch_extensions, &exclude_dirs, tx.clone())
        .await?;

    // Build queue
    let mut queue = BuildQueue::new();

    // Heartbeat ticker
    let mut heartbeat_interval = interval(Duration::from_secs(5));
    let mut shutdown_interval = interval(Duration::from_millis(250));
    let mut config_poll_interval = interval(Duration::from_secs(2));
    let mut reconnect_backoff_secs = 1u64;
    let mut config = config;
    let mut config_mtime = std::fs::metadata(crate::config::config_path(&project_root))
        .and_then(|m| m.modified())
        .ok();

    // Settling delay timer
    let mut settling_delay = Duration::from_millis(config.settling_delay_ms);
    let mut settling_deadline: Option<tokio::time::Instant> = None;

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    {
        let shutdown_flag = Arc::clone(&shutdown_flag);
        ctrlc::set_handler(move || {
            shutdown_flag.store(true, Ordering::SeqCst);
        })
        .context("Failed to install Ctrl+C handler")?;
    }

    println!("👻 BuildWatch is haunting {:?}", project_root);
    println!(
        "   Watching {} target(s). Press Ctrl+C to stop.",
        config.targets.len()
    );

    // Main event loop
    loop {
        tokio::select! {
            _ = shutdown_interval.tick() => {
                if shutdown_flag.load(Ordering::SeqCst) {
                    tracing::info!("Shutdown signal received");
                    break;
                }
            }

            // Heartbeat
            _ = heartbeat_interval.tick() => {
                if let Err(e) = state::update_heartbeat(&state_dir) {
                    tracing::warn!("Failed to update heartbeat: {}", e);
                }
            }

            // Config polling hot-reload
            _ = config_poll_interval.tick() => {
                let new_mtime = std::fs::metadata(crate::config::config_path(&project_root))
                    .and_then(|m| m.modified())
                    .ok();
                if new_mtime.is_some() && new_mtime != config_mtime {
                    match crate::config::load_config(&project_root) {
                        Ok(new_config) => {
                            config = new_config;
                            settling_delay = Duration::from_millis(config.settling_delay_ms);
                            config_mtime = new_mtime;
                            tracing::info!("Reloaded config from buildwatch.config.json");
                        }
                        Err(e) => tracing::warn!("Failed to reload config: {}", e),
                    }
                }
            }

            // File change events from Watchman
            event = rx.recv() => {
                match event {
                    Some(change_event) => {
                        reconnect_backoff_secs = 1;
                        queue.enqueue_from_event(&change_event, &config.targets);
                        // Reset settling timer
                        settling_deadline = Some(
                            tokio::time::Instant::now() + settling_delay
                        );
                    }
                    None => {
                        tracing::warn!("Watcher channel closed, reconnecting...");
                        let mut reconnected = false;
                        for _ in 0..8 {
                            tokio::time::sleep(Duration::from_secs(reconnect_backoff_secs)).await;
                            match Watcher::connect(&project_root).await {
                                Ok(new_watcher) => {
                                    let (new_tx, new_rx) = mpsc::channel(100);
                                    if new_watcher
                                        .subscribe(&watch_extensions, &exclude_dirs, new_tx.clone())
                                        .await
                                        .is_ok()
                                    {
                                        watcher = new_watcher;
                                        rx = new_rx;
                                        reconnect_backoff_secs = 1;
                                        reconnected = true;
                                        tracing::info!("Watchman reconnect succeeded");
                                        break;
                                    }
                                }
                                Err(e) => tracing::warn!("Reconnect attempt failed: {}", e),
                            }
                            reconnect_backoff_secs = (reconnect_backoff_secs * 2).min(30);
                        }
                        if !reconnected {
                            tracing::error!("Failed to reconnect to Watchman");
                            break;
                        }
                    }
                }
            }

            // Settling timer — execute builds after debounce period
            _ = async {
                if let Some(deadline) = settling_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    // No deadline set — sleep forever (will be interrupted by other branches)
                    std::future::pending::<()>().await;
                }
            } => {
                settling_deadline = None;

                // Drain the queue
                while let Some(pending) = queue.dequeue() {
                    // Update queue state
                    let mut ps = state::read_state(&state_dir).unwrap_or_else(|_| initial_state.clone());
                    ps.queue.current = Some(pending.target_name.clone());
                    ps.queue.pending = queue.pending_targets();
                    ps.updated_at = Utc::now();
                    state::write_state(&state_dir, &ps).ok();

                    // Execute the build
                    let result = builder::execute_build(
                        &project_root,
                        &state_dir,
                        &config,
                        &watcher,
                        &pending,
                    )
                    .await;

                    queue.build_complete();

                    // Dispatch notification
                    match &result {
                        Ok(build_result) => {
                            let success = build_result.exit_code == 0;
                            if (success && config.notifications.on_success)
                                || (!success && config.notifications.on_failure)
                            {
                                notifier::notify(
                                    &pending.target_name,
                                    success,
                                    build_result.duration_ms,
                                    build_result.error_summary.as_deref(),
                                    config.notifications.sound,
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("Build error for '{}': {}", pending.target_name, e);
                            notifier::notify(&pending.target_name, false, 0, Some(&e.to_string()), config.notifications.sound);
                        }
                    }

                    // Clear queue state
                    if let Ok(mut ps) = state::read_state(&state_dir) {
                        ps.queue.current = None;
                        ps.queue.pending = queue.pending_targets();
                        ps.updated_at = Utc::now();
                        state::write_state(&state_dir, &ps).ok();
                    }
                }
            }
        }
    }

        Ok(())
    }
    .await;

    state::unregister_project(&project_root).ok();
    state::release_lock(&state_dir).ok();
    run_result
}

/// Start the daemon as a background process.
///
/// For v0.1, this simply spawns the current binary with `--foreground`
/// in a detached child process. Full daemonization (fork/setsid) is
/// a future enhancement.
pub fn run_daemon(
    project_root: PathBuf,
    target_filters: &[String],
    settling: Option<u64>,
) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("haunt")
        .arg("--foreground")
        .arg("--project")
        .arg(&project_root);
    for target in target_filters {
        cmd.arg("--target").arg(target);
    }
    if let Some(ms) = settling {
        cmd.arg("--settling").arg(ms.to_string());
    }

    // Detach the child process
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    let child = cmd.spawn().context("Failed to spawn daemon process")?;
    println!(
        "👻 BuildWatch daemon started (pid {}) for {:?}",
        child.id(),
        project_root
    );
    println!("   Check status: buildwatch status");
    println!("   Stop:         buildwatch stop");

    Ok(())
}

/// Stop the daemon for a given project root.
pub fn stop_daemon(project_root: &Path) -> Result<()> {
    let state_dir = state::state_dir_for(project_root);
    let info = state::read_daemon_info(&state_dir)
        .context("No daemon state found. Is a daemon running?")?;

    if !state::is_daemon_alive(&info) {
        tracing::info!("Daemon already stopped (stale state). Cleaning up.");
        state::clean_state(project_root)?;
        return Ok(());
    }

    // Send SIGTERM (Unix) or TerminateProcess (Windows)
    tracing::info!("Stopping daemon (pid {})...", info.pid);

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(info.pid as i32, libc::SIGTERM);
        }
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &info.pid.to_string(), "/F"])
            .output();
    }

    // Wait briefly for cleanup, then remove state
    std::thread::sleep(std::time::Duration::from_millis(500));
    state::unregister_project(project_root).ok();
    state::release_lock(&state_dir).ok();
    state::clean_state(project_root)?;

    println!("BuildWatch daemon stopped.");
    Ok(())
}

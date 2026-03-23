//! `wr` — Freshness gate runner.
//!
//! Blocks until the target build is ready, then exec's the binary.
//! Reads state files only — never writes, never starts a daemon.

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "wr",
    about = "Run a fresh build artifact — blocks until build is ready",
    version
)]
struct Cli {
    /// Target name (supports fuzzy matching)
    target: String,

    /// Maximum seconds to wait for build
    #[arg(long, default_value_t = 600)]
    timeout: u64,

    /// Don't wait for in-progress builds; fail if not ready
    #[arg(long, default_value_t = false)]
    no_wait: bool,

    /// Force JSON output
    #[arg(long, default_value_t = false)]
    json: bool,

    /// Project root directory
    #[arg(long)]
    project: Option<PathBuf>,

    /// Arguments passed through to the target binary
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let is_agent = buildwatch::output::is_agent_caller();

    let project_root = cli
        .project
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    // Resolve project hash and state directory
    let state_dir = buildwatch::state::state_dir_for(&project_root);

    // Check daemon liveness
    match buildwatch::state::read_daemon_info(&state_dir) {
        Ok(info) if buildwatch::state::is_daemon_alive(&info) => info,
        _ => {
            if is_agent || cli.json {
                let msg = serde_json::json!({
                    "error": "no_daemon",
                    "hint": "Run 'buildwatch haunt' to start the file watcher daemon",
                    "project_root": project_root.display().to_string(),
                });
                println!("{}", serde_json::to_string(&msg)?);
            } else {
                eprintln!("No BuildWatch daemon running for {:?}", project_root);
                eprintln!("Start one with: buildwatch haunt");
            }
            std::process::exit(1);
        }
    };

    // Resolve target with fuzzy matching
    let state = buildwatch::state::read_state(&state_dir)?;
    let resolved_target = buildwatch::state::fuzzy_match_target(&cli.target, &state)
        .context("Target resolution failed")?;

    let deadline = Instant::now() + Duration::from_secs(cli.timeout);

    loop {
        let state = buildwatch::state::read_state(&state_dir)?;
        let target_state = state
            .targets
            .get(&resolved_target)
            .context(format!("Target '{}' not found in state", resolved_target))?;

        match target_state.status.as_str() {
            "ready" => {
                if let Some(ref last_build) = target_state.last_build {
                    if last_build.exit_code == 0 {
                        // Fresh and successful — exec the binary
                        let output_path = last_build
                            .output_path
                            .as_ref()
                            .context("Target has no output_path")?;

                        let full_path = project_root.join(output_path);
                        let mut cmd = Command::new(&full_path);
                        cmd.args(&cli.args);

                        let status = cmd
                            .status()
                            .context(format!("Failed to execute {}", full_path.display()))?;

                        std::process::exit(status.code().unwrap_or(1));
                    } else {
                        // Last build failed
                        if let Some(ref err) = last_build.error_summary {
                            eprintln!("Build failed (exit {}): {}", last_build.exit_code, err);
                        } else {
                            eprintln!("Build failed with exit code {}", last_build.exit_code);
                        }
                        std::process::exit(last_build.exit_code);
                    }
                } else {
                    eprintln!("Target '{}' has no build history yet", resolved_target);
                    eprintln!("Trigger a build with: buildwatch build {}", resolved_target);
                    std::process::exit(1);
                }
            }
            "building" => {
                if cli.no_wait {
                    eprintln!(
                        "Target '{}' is currently building (--no-wait)",
                        resolved_target
                    );
                    std::process::exit(1);
                }
                if Instant::now() > deadline {
                    bail!(
                        "Timeout waiting for '{}' to finish building",
                        resolved_target
                    );
                }
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            "failed" => {
                if let Some(ref last_build) = target_state.last_build {
                    if let Some(ref err) = last_build.error_summary {
                        eprintln!("Build failed (exit {}): {}", last_build.exit_code, err);
                    } else {
                        eprintln!("Build failed with exit code {}", last_build.exit_code);
                    }
                    std::process::exit(last_build.exit_code);
                } else {
                    eprintln!("Target '{}' is in failed state", resolved_target);
                    std::process::exit(1);
                }
            }
            "pending" => {
                if cli.no_wait {
                    eprintln!("Target '{}' is pending build (--no-wait)", resolved_target);
                    std::process::exit(1);
                }
                if Instant::now() > deadline {
                    bail!("Timeout waiting for '{}' build", resolved_target);
                }
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            other => {
                bail!("Unknown target status: {}", other);
            }
        }
    }
}

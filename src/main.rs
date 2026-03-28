//! BuildWatch CLI — the `buildwatch` binary entry point.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "buildwatch",
    about = "Universal file watcher & build daemon — keeping your builds fresh",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Project root directory (defaults to current directory)
    #[arg(long, global = true)]
    project: Option<PathBuf>,

    /// Verbose output
    #[arg(long, global = true, default_value_t = false)]
    verbose: bool,

    /// Force JSON output
    #[arg(long, global = true, default_value_t = false)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Auto-detect project and generate buildwatch.config.json
    Init {
        /// Accept all defaults without prompting
        #[arg(long, default_value_t = false)]
        auto: bool,

        /// Override detected project type
        #[arg(long, name = "type")]
        project_type: Option<String>,
    },

    /// Start watching and auto-building (daemon mode)
    Watch {
        /// Run in foreground (don't daemonize)
        #[arg(long, default_value_t = false)]
        foreground: bool,

        /// Only watch/build specific target(s)
        #[arg(long)]
        target: Vec<String>,

        /// Override settling delay (milliseconds)
        #[arg(long)]
        settling: Option<u64>,
    },

    /// Alias for watch
    Start {
        #[arg(long, default_value_t = false)]
        foreground: bool,
    },

    /// Stop the daemon for this project
    Stop,

    /// Show status of all active daemons
    Status {
        /// Show detailed build statistics
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },

    /// Trigger a manual build
    Build {
        /// Target name (optional, builds all if omitted)
        target: Option<String>,
    },

    /// Remove state files and stop daemon
    Clean,

    /// Show resolved configuration
    Config,

    /// Tail the build log for a target
    Log {
        /// Target name
        target: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    let subscriber = tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if cli.verbose {
                "buildwatch=debug".into()
            } else {
                "buildwatch=info".into()
            }
        }),
    );

    if cli.json {
        subscriber.json().init();
    } else {
        subscriber.init();
    }

    let project_root = cli
        .project
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    match cli.command {
        Commands::Init { auto, project_type } => {
            tracing::info!("Detecting project at {:?}", project_root);
            let detected = buildwatch::detector::detect_project(&project_root)?;
            let config = buildwatch::detector::generate_config(detected, project_type)?;
            buildwatch::config::write_config(&project_root, &config)?;
            if !auto {
                println!(
                    "Generated buildwatch.config.json with {} target(s)",
                    config.targets.len()
                );
                println!("Review the config, then run: buildwatch watch");
            }
        }
        Commands::Watch {
            foreground,
            target,
            settling,
        } => {
            let mut config = buildwatch::config::load_config(&project_root)?;
            buildwatch::config::apply_watch_overrides(&mut config, &target, settling)?;
            if foreground {
                buildwatch::daemon::run_foreground(project_root, config).await?;
            } else {
                buildwatch::daemon::run_daemon(project_root, &target, settling)?;
            }
        }
        Commands::Start { foreground } => {
            let config = buildwatch::config::load_config(&project_root)?;
            if foreground {
                buildwatch::daemon::run_foreground(project_root, config).await?;
            } else {
                buildwatch::daemon::run_daemon(project_root, &[], None)?;
            }
        }
        Commands::Stop => {
            buildwatch::daemon::stop_daemon(&project_root)?;
        }
        Commands::Status { verbose } => {
            buildwatch::state::print_status(verbose, cli.json)?;
        }
        Commands::Build { target } => {
            let config = buildwatch::config::load_config(&project_root)?;
            buildwatch::builder::manual_build(&project_root, &config, target.as_deref()).await?;
        }
        Commands::Clean => {
            buildwatch::daemon::stop_daemon(&project_root).ok();
            buildwatch::state::clean_state(&project_root)?;
            println!("Cleaned state for {:?}", project_root);
        }
        Commands::Config => {
            let config = buildwatch::config::load_config(&project_root)?;
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Commands::Log { target } => {
            buildwatch::state::tail_log(&project_root, target.as_deref())?;
        }
    }

    Ok(())
}

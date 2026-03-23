//! Watchman integration layer.
//!
//! Manages the connection to Watchman, subscriptions, and
//! state assertions for build coordination.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use watchman_client::prelude::*;
use watchman_client::SubscriptionData;

/// Events emitted by the watcher to the build queue.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    /// Files that changed (relative to project root)
    pub changed_files: Vec<String>,
    /// Timestamp of the change batch
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Manages the Watchman connection and subscriptions.
pub struct Watcher {
    client: Client,
    root: ResolvedRoot,
    project_root: PathBuf,
}

// Define the fields we want from Watchman query results
watchman_client::query_result_type! {
    pub struct WatchmanFile {
        pub name: NameField,
        pub file_type: FileTypeField,
        pub exists: ExistsField,
    }
}

impl Watcher {
    /// Connect to Watchman and resolve the project root.
    pub async fn connect(project_root: &Path) -> Result<Self> {
        tracing::info!("Connecting to Watchman...");
        let client = Connector::new()
            .connect()
            .await
            .context("Failed to connect to Watchman. Is it running? Install: https://facebook.github.io/watchman/docs/install")?;

        let canonical = CanonicalPath::canonicalize(project_root)
            .context("Failed to canonicalize project root")?;

        tracing::info!("Resolving root: {:?}", project_root);
        let root = client
            .resolve_root(canonical)
            .await
            .context("Failed to resolve Watchman root")?;

        Ok(Self {
            client,
            root,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Subscribe to file changes matching the given extensions.
    /// Sends FileChangeEvent messages to the provided channel.
    pub async fn subscribe(
        &self,
        watch_extensions: &[String],
        exclude_dirs: &[String],
        tx: mpsc::Sender<FileChangeEvent>,
    ) -> Result<()> {
        // Build suffix expression for file matching
        let suffix_exprs: Vec<Expr> = watch_extensions
            .iter()
            .map(|ext| {
                let ext = ext.strip_prefix('.').unwrap_or(ext);
                Expr::Suffix(vec![ext.to_string().into()])
            })
            .collect();

        let match_expr = if suffix_exprs.is_empty() {
            Expr::True
        } else {
            Expr::Any(suffix_exprs)
        };

        // Build exclude expression for directories
        let exclude_expr = Expr::Not(Box::new(Expr::Any(
            exclude_dirs
                .iter()
                .map(|d| {
                    let d = d.strip_suffix('/').unwrap_or(d);
                    Expr::DirName(DirNameTerm {
                        path: d.into(),
                        depth: None,
                    })
                })
                .collect(),
        )));

        let full_expr = Expr::All(vec![match_expr, exclude_expr]);

        // Create subscription
        let (mut subscription, _) = self
            .client
            .subscribe::<WatchmanFile>(
                &self.root,
                SubscribeRequest {
                    expression: Some(full_expr),
                    fields: vec!["name", "type", "exists"],
                    defer: vec!["buildwatch.build"],
                    ..Default::default()
                },
            )
            .await
            .context("Failed to create Watchman subscription")?;

        tracing::info!("Subscription active for {:?}", self.project_root);

        // Event loop: read subscription events and forward to channel
        tokio::spawn(async move {
            loop {
                match subscription.next().await {
                    Ok(event) => match event {
                        SubscriptionData::FilesChanged(query_result) => {
                            let files: Vec<String> = query_result
                                .files
                                .unwrap_or_default()
                                .iter()
                                .map(|f| f.name.to_string_lossy().to_string())
                                .collect();

                            if !files.is_empty() {
                                tracing::debug!("Files changed: {:?}", files);
                                let event = FileChangeEvent {
                                    changed_files: files,
                                    timestamp: chrono::Utc::now(),
                                };
                                if tx.send(event).await.is_err() {
                                    tracing::warn!("Channel closed, stopping watcher");
                                    break;
                                }
                            }
                        }
                        SubscriptionData::StateEnter { .. } => {
                            tracing::trace!("State entered (build in progress)");
                        }
                        SubscriptionData::StateLeave { .. } => {
                            tracing::trace!("State left (build complete)");
                        }
                        SubscriptionData::Canceled => {
                            tracing::info!("Subscription canceled");
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::error!("Watchman subscription error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Assert the "buildwatch.build" state to defer subscription events during builds.
    pub async fn state_enter(&self) -> Result<()> {
        self.client
            .state_enter(&self.root, "buildwatch.build", SyncTimeout::Default, None)
            .await
            .context("Failed to assert buildwatch.build state")?;
        Ok(())
    }

    /// Release the "buildwatch.build" state so deferred events are delivered.
    pub async fn state_leave(&self) -> Result<()> {
        self.client
            .state_leave(&self.root, "buildwatch.build", SyncTimeout::Default, None)
            .await
            .context("Failed to release buildwatch.build state")?;
        Ok(())
    }
}

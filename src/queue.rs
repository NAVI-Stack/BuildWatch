//! Build queue with priority scoring and coalescing.
//!
//! Only one build runs at a time per daemon. Changes that arrive during
//! a build are coalesced into a single pending entry per target.

use crate::config::TargetConfig;
use crate::watcher::FileChangeEvent;
use std::collections::HashMap;

/// A pending build request with accumulated change context.
#[derive(Debug, Clone)]
pub struct PendingBuild {
    pub target_name: String,
    pub priority_score: i32,
    pub trigger_files: Vec<String>,
    pub queued_at: chrono::DateTime<chrono::Utc>,
}

/// Manages build ordering and coalescing.
pub struct BuildQueue {
    /// Pending builds keyed by target name (at most one per target).
    pending: HashMap<String, PendingBuild>,
    /// Whether a build is currently running.
    pub building: bool,
    /// Name of the currently building target.
    pub current_target: Option<String>,
}

impl BuildQueue {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            building: false,
            current_target: None,
        }
    }

    /// Evaluate a file change event against configured targets.
    /// Enqueue affected targets, coalescing if already pending.
    pub fn enqueue_from_event(
        &mut self,
        event: &FileChangeEvent,
        targets: &[TargetConfig],
    ) {
        for target in targets {
            if !target.enabled {
                continue;
            }

            let affected = event.changed_files.iter().any(|f| {
                // Check if file matches target's watch extensions
                target.watch_extensions.iter().any(|ext| {
                    let ext = ext.strip_prefix('.').unwrap_or(ext);
                    f.ends_with(ext)
                })
            });

            if !affected && !target.watch_extensions.is_empty() {
                continue;
            }

            if let Some(existing) = self.pending.get_mut(&target.name) {
                // Coalesce: merge trigger files into existing pending build
                for f in &event.changed_files {
                    if !existing.trigger_files.contains(f) {
                        existing.trigger_files.push(f.clone());
                    }
                }
                tracing::debug!("Coalesced change into pending build for '{}'", target.name);
            } else {
                // New pending build
                let score = compute_priority(target, &event.changed_files);
                self.pending.insert(
                    target.name.clone(),
                    PendingBuild {
                        target_name: target.name.clone(),
                        priority_score: score,
                        trigger_files: event.changed_files.clone(),
                        queued_at: event.timestamp,
                    },
                );
                tracing::info!("Queued build for '{}' (priority {})", target.name, score);
            }
        }
    }

    /// Dequeue the highest-priority pending build.
    /// Returns None if the queue is empty or a build is already running.
    pub fn dequeue(&mut self) -> Option<PendingBuild> {
        if self.building || self.pending.is_empty() {
            return None;
        }

        // Find the highest priority pending build
        let best_target = self
            .pending
            .values()
            .max_by_key(|p| p.priority_score)
            .map(|p| p.target_name.clone());

        if let Some(target_name) = best_target {
            let build = self.pending.remove(&target_name);
            if build.is_some() {
                self.building = true;
                self.current_target = Some(target_name);
            }
            build
        } else {
            None
        }
    }

    /// Mark the current build as complete. Allows the next dequeue.
    pub fn build_complete(&mut self) {
        self.building = false;
        self.current_target = None;
    }

    /// Check if there are pending builds waiting.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Get a snapshot of pending target names for state reporting.
    pub fn pending_targets(&self) -> Vec<String> {
        let mut targets: Vec<_> = self.pending.values().collect();
        targets.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));
        targets.iter().map(|p| p.target_name.clone()).collect()
    }
}

/// Compute priority score for a target based on config and changed files.
fn compute_priority(target: &TargetConfig, changed_files: &[String]) -> i32 {
    let mut score = target.priority;

    // File affinity: boost if changed files fall within watch_paths
    let affinity = changed_files.iter().filter(|f| {
        target.watch_paths.is_empty()
            || target.watch_paths.iter().any(|wp| f.starts_with(wp))
    }).count();

    score += affinity as i32;

    score
}

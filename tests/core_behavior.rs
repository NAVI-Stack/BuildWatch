use buildwatch::config::{apply_watch_overrides, Config, NotificationConfig, TargetConfig};
use buildwatch::queue::BuildQueue;
use buildwatch::watcher::FileChangeEvent;
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;

fn make_target(name: &str, ext: &str, priority: i32) -> TargetConfig {
    TargetConfig {
        name: name.to_string(),
        build_command: "echo build".to_string(),
        output_path: None,
        working_directory: ".".to_string(),
        watch_extensions: vec![ext.to_string()],
        watch_paths: vec!["src/".to_string()],
        exclude_paths: vec![],
        environment: HashMap::new(),
        priority,
        enabled: true,
        post_build: None,
        auto_restart: false,
    }
}

#[test]
fn project_hash_is_stable_for_same_path() {
    let p = Path::new(".");
    let h1 = buildwatch::project_hash(p);
    let h2 = buildwatch::project_hash(p);
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 16);
}

#[test]
fn queue_coalesces_changes_per_target() {
    let targets = vec![make_target("api", ".rs", 5)];
    let mut queue = BuildQueue::new();
    let first = FileChangeEvent {
        changed_files: vec!["src/main.rs".to_string()],
        timestamp: Utc::now(),
    };
    let second = FileChangeEvent {
        changed_files: vec!["src/lib.rs".to_string()],
        timestamp: Utc::now(),
    };

    queue.enqueue_from_event(&first, &targets);
    queue.enqueue_from_event(&second, &targets);

    let pending = queue.dequeue().expect("pending build should exist");
    assert_eq!(pending.target_name, "api");
    assert_eq!(pending.trigger_files.len(), 2);
}

#[test]
fn queue_prioritizes_higher_score_target() {
    let targets = vec![make_target("low", ".rs", 1), make_target("high", ".rs", 10)];
    let mut queue = BuildQueue::new();
    let event = FileChangeEvent {
        changed_files: vec!["src/main.rs".to_string()],
        timestamp: Utc::now(),
    };
    queue.enqueue_from_event(&event, &targets);
    let first = queue.dequeue().expect("first target should dequeue");
    assert_eq!(first.target_name, "high");
}

#[test]
fn watch_overrides_filter_targets_and_set_settling() {
    let mut config = Config {
        version: 1,
        settling_delay_ms: 200,
        build_timeout_seconds: 300,
        notifications: NotificationConfig::default(),
        targets: vec![make_target("api", ".rs", 5), make_target("web", ".ts", 3)],
        global_excludes: vec![],
    };
    apply_watch_overrides(&mut config, &[String::from("web")], Some(750))
        .expect("overrides should apply");
    assert_eq!(config.settling_delay_ms, 750);
    assert_eq!(config.targets.len(), 1);
    assert_eq!(config.targets[0].name, "web");
}

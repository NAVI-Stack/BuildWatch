//! Project type auto-detection from filesystem markers.
//!
//! Scans a directory for build system files and generates a Config
//! with appropriate targets and defaults.

use crate::config::{Config, NotificationConfig, TargetConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// A detected project type with its configuration defaults.
#[derive(Debug, Clone)]
pub struct DetectedProject {
    pub project_type: String,
    pub name: String,
    pub build_command: String,
    pub output_path: Option<String>,
    pub watch_extensions: Vec<String>,
    pub exclude_paths: Vec<String>,
}

/// Scan a directory and detect all project types present.
pub fn detect_project(project_root: &Path) -> Result<Vec<DetectedProject>> {
    let mut detected = Vec::new();

    // Derive a project name from the directory
    let dir_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    // Rust / Cargo
    if project_root.join("Cargo.toml").exists() {
        detected.push(DetectedProject {
            project_type: "rust".into(),
            name: dir_name.clone(),
            build_command: "cargo build".into(),
            output_path: Some(format!("target/debug/{}", dir_name)),
            watch_extensions: vec![".rs".into(), ".toml".into()],
            exclude_paths: vec!["target/".into()],
        });
    }

    // Go
    if project_root.join("go.mod").exists() {
        detected.push(DetectedProject {
            project_type: "go".into(),
            name: dir_name.clone(),
            build_command: format!("go build -o bin/{} ./cmd/{}", dir_name, dir_name),
            output_path: Some(format!("bin/{}", dir_name)),
            watch_extensions: vec![".go".into(), ".mod".into(), ".sum".into()],
            exclude_paths: vec!["vendor/".into(), "bin/".into()],
        });
    }

    // Swift
    if project_root.join("Package.swift").exists() {
        detected.push(DetectedProject {
            project_type: "swift".into(),
            name: dir_name.clone(),
            build_command: "swift build".into(),
            output_path: Some(format!(".build/debug/{}", dir_name)),
            watch_extensions: vec![".swift".into()],
            exclude_paths: vec![".build/".into()],
        });
    }

    // TypeScript / Node
    if project_root.join("package.json").exists() && project_root.join("tsconfig.json").exists() {
        detected.push(DetectedProject {
            project_type: "typescript".into(),
            name: dir_name.clone(),
            build_command: "npm run build".into(),
            output_path: Some("dist/".into()),
            watch_extensions: vec![".ts".into(), ".tsx".into(), ".js".into(), ".jsx".into()],
            exclude_paths: vec!["node_modules/".into(), "dist/".into()],
        });
    }

    // CMake
    if project_root.join("CMakeLists.txt").exists() {
        detected.push(DetectedProject {
            project_type: "cmake".into(),
            name: dir_name.clone(),
            build_command: "cmake --build build".into(),
            output_path: None,
            watch_extensions: vec![
                ".c".into(),
                ".cpp".into(),
                ".h".into(),
                ".hpp".into(),
                ".cmake".into(),
            ],
            exclude_paths: vec!["build/".into()],
        });
    }

    // Make
    if project_root.join("Makefile").exists() || project_root.join("makefile").exists() {
        // Only add if no more specific build system was detected
        if detected.is_empty() {
            detected.push(DetectedProject {
                project_type: "make".into(),
                name: dir_name.clone(),
                build_command: "make".into(),
                output_path: None,
                watch_extensions: vec![], // watch all
                exclude_paths: vec![],
            });
        }
    }

    // Docker
    if project_root.join("Dockerfile").exists() {
        detected.push(DetectedProject {
            project_type: "docker".into(),
            name: format!("{}-docker", dir_name),
            build_command: "docker build .".into(),
            output_path: None,
            watch_extensions: vec!["Dockerfile".into()],
            exclude_paths: vec![".git/".into()],
        });
    }

    // .NET
    let has_csproj = std::fs::read_dir(project_root)
        .ok()
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.ends_with(".csproj") || name.ends_with(".sln")
            })
        })
        .unwrap_or(false);

    if has_csproj {
        detected.push(DetectedProject {
            project_type: "dotnet".into(),
            name: dir_name.clone(),
            build_command: "dotnet build".into(),
            output_path: None,
            watch_extensions: vec![".cs".into(), ".csproj".into(), ".sln".into()],
            exclude_paths: vec!["bin/".into(), "obj/".into()],
        });
    }

    // Python (no build command by default — interpreted)
    if project_root.join("pyproject.toml").exists() || project_root.join("setup.py").exists() {
        detected.push(DetectedProject {
            project_type: "python".into(),
            name: dir_name.clone(),
            build_command: "echo 'Python: no build step'".into(),
            output_path: None,
            watch_extensions: vec![".py".into()],
            exclude_paths: vec!["__pycache__/".into(), ".venv/".into(), "venv/".into()],
        });
    }

    if detected.is_empty() {
        tracing::warn!("No recognized project type detected in {:?}", project_root);
        tracing::info!("You can manually create buildwatch.config.json");
    } else {
        tracing::info!(
            "Detected {} project type(s): {}",
            detected.len(),
            detected
                .iter()
                .map(|d| d.project_type.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(detected)
}

/// Generate a Config from detected projects, with optional type override.
pub fn generate_config(
    detected: Vec<DetectedProject>,
    type_override: Option<String>,
) -> Result<Config> {
    let targets: Vec<TargetConfig> = detected
        .into_iter()
        .filter(|d| {
            type_override
                .as_ref()
                .map(|t| d.project_type == *t)
                .unwrap_or(true)
        })
        .map(|d| TargetConfig {
            name: d.name,
            build_command: d.build_command,
            output_path: d.output_path,
            working_directory: ".".into(),
            watch_extensions: d.watch_extensions,
            watch_paths: vec![],
            exclude_paths: d.exclude_paths,
            environment: HashMap::new(),
            priority: 5,
            enabled: true,
            post_build: None,
            auto_restart: false,
        })
        .collect();

    Ok(Config {
        version: 1,
        settling_delay_ms: 200,
        build_timeout_seconds: 300,
        notifications: NotificationConfig::default(),
        targets,
        global_excludes: vec![
            ".git/".into(),
            "node_modules/".into(),
            "__pycache__/".into(),
            "target/".into(),
            "bin/".into(),
            ".next/".into(),
            "dist/".into(),
        ],
    })
}

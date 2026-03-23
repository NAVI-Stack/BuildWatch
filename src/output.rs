//! Terminal formatting and agent detection.
//!
//! Determines whether the caller is a human or an AI agent and
//! adjusts output format accordingly.

use std::env;

/// Known environment variables that indicate an AI agent caller.
const AGENT_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE",
    "CURSOR_SESSION",
    "CODEX_SESSION",
    "NAVI_AGENT",
    "HELM_SESSION",
    "AIDER_SESSION",
    "BUILDWATCH_AGENT_MODE",
];

/// Detect whether the current caller is likely an AI agent.
///
/// Checks for known agent environment variables and terminal
/// interactivity. Returns true if any agent marker is found
/// or if stdout is not a TTY.
pub fn is_agent_caller() -> bool {
    // Explicit agent mode flag
    for var in AGENT_ENV_VARS {
        if env::var(var).is_ok() {
            return true;
        }
    }

    // Non-interactive terminal heuristic
    // (piped output, cron, CI, etc.)
    if !atty::is(atty::Stream::Stdout) {
        return true;
    }

    false
}

/// Format a build status line for human or agent consumption.
pub fn format_status(target: &str, status: &str, duration_ms: Option<u64>, agent: bool) -> String {
    if agent {
        // Structured JSON for agents
        let json = serde_json::json!({
            "target": target,
            "status": status,
            "duration_ms": duration_ms,
        });
        serde_json::to_string(&json).unwrap_or_default()
    } else {
        // Human-friendly colored output
        let icon = match status {
            "ready" => "✓",
            "building" => "⟳",
            "failed" => "✗",
            "pending" => "○",
            "stale" => "?",
            _ => "-",
        };

        let duration_str = duration_ms
            .map(|d| format!(" ({:.1}s)", d as f64 / 1000.0))
            .unwrap_or_default();

        format!("{} {} [{}]{}", icon, target, status, duration_str)
    }
}

/// Format an error message for agent consumption.
/// Agents get structured JSON; humans get a readable message.
pub fn format_error(error_type: &str, message: &str, hint: Option<&str>, agent: bool) -> String {
    if agent {
        let mut json = serde_json::json!({
            "error": error_type,
            "message": message,
        });
        if let Some(h) = hint {
            json["hint"] = serde_json::Value::String(h.to_string());
        }
        serde_json::to_string(&json).unwrap_or_default()
    } else {
        match hint {
            Some(h) => format!("{}\n  Hint: {}", message, h),
            None => message.to_string(),
        }
    }
}

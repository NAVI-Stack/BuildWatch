//! System notification dispatch.
//!
//! Sends platform-native notifications for build events.

use notify_rust::Notification;

/// Send a build notification.
pub fn notify(
    target_name: &str,
    success: bool,
    duration_ms: u64,
    error_summary: Option<&str>,
    _sound: bool,
) {
    let (title, body) = if success {
        (
            format!("✓ {}", target_name),
            format!("Built in {:.1}s", duration_ms as f64 / 1000.0),
        )
    } else {
        let detail = error_summary.unwrap_or("Unknown error");
        // Truncate error for notification display
        let short_error: String = detail
            .lines()
            .next()
            .unwrap_or(detail)
            .chars()
            .take(100)
            .collect();
        (
            format!("✗ {}", target_name),
            format!("Build failed: {}", short_error),
        )
    };

    if let Err(e) = send_notification(&title, &body) {
        tracing::debug!("Notification failed (non-critical): {}", e);
    }
}

/// Platform-specific notification dispatch.
fn send_notification(title: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    Notification::new()
        .summary(&format!("BuildWatch: {title}"))
        .body(body)
        .show()?;
    Ok(())
}

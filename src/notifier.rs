//! System notification dispatch.
//!
//! Sends platform-native notifications for build events.

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
        let short_error: String = detail.lines().next().unwrap_or(detail).chars().take(100).collect();
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
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "display notification \"{}\" with title \"BuildWatch\" subtitle \"{}\"",
                    body.replace('"', "\\\""),
                    title.replace('"', "\\\"")
                ),
            ])
            .output()?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("notify-send")
            .args([&format!("BuildWatch: {}", title), body])
            .output()?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        // PowerShell toast notification
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
             $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $textNodes = $template.GetElementsByTagName('text'); \
             $textNodes.Item(0).AppendChild($template.CreateTextNode('BuildWatch: {}')) | Out-Null; \
             $textNodes.Item(1).AppendChild($template.CreateTextNode('{}')) | Out-Null; \
             $toast = [Windows.UI.Notifications.ToastNotification]::new($template); \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('BuildWatch').Show($toast)",
            title.replace('\'', "''"),
            body.replace('\'', "''")
        );
        std::process::Command::new("powershell")
            .args(["-Command", &script])
            .output()?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        tracing::debug!("Notifications not supported on this platform");
        Ok(())
    }
}

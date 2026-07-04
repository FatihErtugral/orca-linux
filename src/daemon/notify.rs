use super::store::Notification;
use crate::protocol::AgentStatus;

/// DIP seam: the store loop talks to this trait so tests can spy and the
/// desktop implementation stays swappable.
pub trait Notifier {
    fn notify(&self, title: &str, body: &str, sound: bool);
}

/// Formats a store notification the way the macOS app words it.
pub fn format(notification: &Notification) -> Option<(String, String)> {
    let (emoji, fallback) = match notification.status {
        AgentStatus::Waiting => ("⏳", "Waiting for you"),
        AgentStatus::Done => ("✅", "Finished"),
        AgentStatus::Error => ("❌", "Error"),
        AgentStatus::Running | AgentStatus::Idle => return None,
    };
    let body = notification
        .message
        .clone()
        .unwrap_or_else(|| fallback.into());
    Some((format!("{emoji} {}", notification.title), body))
}

/// org.freedesktop.Notifications via notify-rust. Fired from a short-lived
/// thread so a slow notification daemon can't stall the store loop.
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&self, title: &str, body: &str, sound: bool) {
        let title = title.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.appname("Orca").summary(&title).body(&body);
            // The server's own sound handling is unreliable across desktops
            // (Plasma ignores sound-name for unregistered apps), so keep the
            // server silent and play the event sound ourselves.
            notification.hint(notify_rust::Hint::SuppressSound(true));
            if sound {
                play_notification_sound();
            }
            let _ = notification.show();
        });
    }
}

/// Event sound via whatever player works; freedesktop's sound theme ships on
/// effectively every desktop install. Players are tried in order and judged
/// by their exit status — merely spawning proves nothing (canberra happily
/// exits 0 without audio under a systemd service environment), so the direct
/// file players come first. Runs on the short-lived notify thread, so
/// blocking on `status()` is fine.
fn play_notification_sound() {
    const THEME_FILE: &str = "/usr/share/sounds/freedesktop/stereo/message.oga";

    // HDMI sinks suspend when idle and swallow the first ~half second of
    // audio while re-waking — exactly the length of a notification sound.
    // Prime the sink with a short burst of silence so the real sound lands
    // on an awake device.
    if let Ok(mut primer) = std::process::Command::new("paplay")
        .args(["--raw", "--rate=48000", "--channels=2", "/dev/zero"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = primer.kill();
        let _ = primer.wait();
    }

    let attempts: [(&str, &[&str]); 3] = [
        ("paplay", &[THEME_FILE]),
        ("pw-play", &[THEME_FILE]),
        ("canberra-gtk-play", &["-i", "message-new-instant"]),
    ];
    for (player, args) in attempts {
        let status = std::process::Command::new(player)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(status, Ok(s) if s.success()) {
            return;
        }
    }
    log::warn!("notification sound: no player succeeded");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(status: AgentStatus, message: Option<&str>) -> Notification {
        Notification {
            status,
            title: "proj".into(),
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn formats_status_emoji_and_default_bodies() {
        assert_eq!(
            format(&notification(AgentStatus::Waiting, None)),
            Some(("⏳ proj".into(), "Waiting for you".into()))
        );
        assert_eq!(
            format(&notification(AgentStatus::Done, None)),
            Some(("✅ proj".into(), "Finished".into()))
        );
        assert_eq!(
            format(&notification(AgentStatus::Error, None)),
            Some(("❌ proj".into(), "Error".into()))
        );
    }

    #[test]
    fn message_overrides_default_body() {
        assert_eq!(
            format(&notification(AgentStatus::Waiting, Some("Your turn"))),
            Some(("⏳ proj".into(), "Your turn".into()))
        );
    }

    #[test]
    fn running_and_idle_never_format() {
        assert_eq!(format(&notification(AgentStatus::Running, None)), None);
        assert_eq!(format(&notification(AgentStatus::Idle, None)), None);
    }
}

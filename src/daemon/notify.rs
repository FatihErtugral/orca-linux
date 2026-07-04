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
    let body = notification.message.clone().unwrap_or_else(|| fallback.into());
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
            if sound {
                notification.hint(notify_rust::Hint::SoundName("message-new-instant".into()));
            } else {
                notification.hint(notify_rust::Hint::SuppressSound(true));
            }
            let _ = notification.show();
        });
    }
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

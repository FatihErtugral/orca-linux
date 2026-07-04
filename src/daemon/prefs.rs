use crate::protocol::AgentStatus;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Notification preferences persisted as `config.toml`. Missing file or keys
/// fall back to everything-on, mirroring the macOS UserDefaults behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub on_waiting: bool,
    pub on_done: bool,
    pub on_error: bool,
    pub sound: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            on_waiting: true,
            on_done: true,
            on_error: true,
            sound: true,
        }
    }
}

impl NotificationPreferences {
    /// Whether a transition into `status` should produce a notification.
    pub fn should_notify(&self, status: AgentStatus) -> bool {
        if !self.enabled {
            return false;
        }
        match status {
            AgentStatus::Waiting => self.on_waiting,
            AgentStatus::Done => self.on_done,
            AgentStatus::Error => self.on_error,
            AgentStatus::Running | AgentStatus::Idle => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub notifications: NotificationPreferences,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let Ok(content) = fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&content).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> bool {
        let Ok(content) = toml::to_string_pretty(self) else {
            return false;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, content).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_all_on() {
        let prefs = NotificationPreferences::default();
        assert!(prefs.should_notify(AgentStatus::Waiting));
        assert!(prefs.should_notify(AgentStatus::Done));
        assert!(prefs.should_notify(AgentStatus::Error));
        assert!(!prefs.should_notify(AgentStatus::Running));
        assert!(!prefs.should_notify(AgentStatus::Idle));
    }

    #[test]
    fn master_switch_suppresses_all() {
        let prefs = NotificationPreferences {
            enabled: false,
            ..Default::default()
        };
        assert!(!prefs.should_notify(AgentStatus::Waiting));
        assert!(!prefs.should_notify(AgentStatus::Error));
    }

    #[test]
    fn per_status_toggle_suppresses_only_that_status() {
        let prefs = NotificationPreferences {
            on_waiting: false,
            ..Default::default()
        };
        assert!(!prefs.should_notify(AgentStatus::Waiting));
        assert!(prefs.should_notify(AgentStatus::Error));
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            notifications: NotificationPreferences {
                on_done: false,
                sound: false,
                ..Default::default()
            },
        };
        assert!(config.save(&path));
        assert_eq!(Config::load(&path), config);
    }

    #[test]
    fn missing_file_and_partial_keys_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("nope.toml")), Config::default());

        let path = dir.path().join("partial.toml");
        fs::write(&path, "[notifications]\nsound = false\n").unwrap();
        let config = Config::load(&path);
        assert!(!config.notifications.sound);
        assert!(config.notifications.enabled);
        assert!(config.notifications.on_waiting);
    }
}

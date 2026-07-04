use crate::daemon::prefs::NotificationPreferences;
use crate::protocol::AgentStatus;
use serde::{Deserialize, Serialize};

/// Snapshot the daemon streams to a subscribed popup process — one JSON line
/// per change over the same unix socket the CLI uses.
#[derive(Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UiState {
    pub rows: Vec<UiAgent>,
    pub running: usize,
    pub open: usize,
    pub prefs: NotificationPreferences,
    /// Daemon version — the plasmoid shows it and uses its change as the
    /// "update finished" signal.
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct UiAgent {
    pub id: String,
    pub title: String,
    pub source: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_started_at: Option<f64>,
    pub last_run_duration: f64,
}

impl UiAgent {
    /// Ticks locally in the popup while running; the daemon only needs to
    /// push when something other than the wall clock changes.
    pub fn duration(&self, now: f64) -> f64 {
        if self.status == AgentStatus::Running {
            if let Some(start) = self.run_started_at {
                return (now - start).max(0.0);
            }
        }
        self.last_run_duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_lines() {
        let state = UiState {
            rows: vec![UiAgent {
                id: "a".into(),
                title: "T".into(),
                source: "claude-code".into(),
                status: AgentStatus::Waiting,
                message: Some("m".into()),
                run_started_at: None,
                last_run_duration: 7.5,
            }],
            running: 0,
            open: 1,
            prefs: NotificationPreferences::default(),
        };
        let line = serde_json::to_string(&state).unwrap();
        assert!(!line.contains('\n'));
        let back: UiState = serde_json::from_str(&line).unwrap();
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].status, AgentStatus::Waiting);
        assert_eq!(back.open, 1);
    }

    #[test]
    fn duration_ticks_only_while_running() {
        let mut agent = UiAgent {
            id: "a".into(),
            title: "t".into(),
            source: "s".into(),
            status: AgentStatus::Running,
            message: None,
            run_started_at: Some(100.0),
            last_run_duration: 7.0,
        };
        assert_eq!(agent.duration(105.0), 5.0);
        agent.status = AgentStatus::Waiting;
        assert_eq!(agent.duration(105.0), 7.0);
    }
}

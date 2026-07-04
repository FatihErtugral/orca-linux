use serde::{Deserialize, Serialize};

/// The wire protocol shared with the CLI side. One event is a single line of
/// JSON. Field names match the macOS implementation byte-for-byte so hook
/// snippets and state files stay interchangeable in shape.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct AgentEvent {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Waiting,
    Done,
    Error,
    Idle,
}

impl AgentStatus {
    /// Unknown statuses fall back to running, mirroring the macOS behavior.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "waiting" => Self::Waiting,
            "done" => Self::Done,
            "error" => Self::Error,
            "idle" => Self::Idle,
            _ => Self::Running,
        }
    }

    /// Menu sort priority — things needing attention float to the top.
    pub fn priority(self) -> u8 {
        match self {
            Self::Waiting => 0,
            Self::Error => 1,
            Self::Running => 2,
            Self::Done => 3,
            Self::Idle => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting for input",
            Self::Done => "done",
            Self::Error => "error",
            Self::Idle => "idle",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_all_fields() {
        let event = AgentEvent {
            id: "abc".into(),
            source: "claude-code".into(),
            title: Some("Fix the tests".into()),
            cwd: Some("/home/user/project".into()),
            status: "running".into(),
            message: Some("hi".into()),
            ts: Some(1_700_000_000.5),
            tty: Some("/dev/pts/3".into()),
            term_program: Some("konsole".into()),
            session: Some("/Sessions/1".into()),
            app_bundle_id: None,
            transcript_path: Some("/tmp/t.jsonl".into()),
            pid: Some(4242),
            permission_mode: Some("default".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn decodes_a_macos_bridge_line_without_key_drift() {
        // Shape captured from the macOS bridge (Swift JSONEncoder with the
        // CodingKeys in AgentEvent.swift). Any key rename breaks compatibility.
        let line = r#"{"id":"s-1","source":"claude-code","title":"T","cwd":"/w","status":"waiting","message":"Response ready, your turn","ts":1719400000.12,"tty":"/dev/ttys003","term_program":"iTerm.app","session":"w0t0p0","app_bundle_id":"com.googlecode.iterm2","transcript_path":"/x.jsonl","pid":123,"permission_mode":"default"}"#;
        let event: AgentEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.id, "s-1");
        assert_eq!(event.term_program.as_deref(), Some("iTerm.app"));
        assert_eq!(
            event.app_bundle_id.as_deref(),
            Some("com.googlecode.iterm2")
        );
        assert_eq!(event.transcript_path.as_deref(), Some("/x.jsonl"));
        assert_eq!(event.permission_mode.as_deref(), Some("default"));
        assert_eq!(event.pid, Some(123));
    }

    #[test]
    fn omits_absent_optionals_when_encoding() {
        let event = AgentEvent {
            id: "x".into(),
            source: "custom".into(),
            status: "running".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("title"));
        assert!(!json.contains("pid"));
        assert!(!json.contains("app_bundle_id"));
    }

    #[test]
    fn ignores_unknown_fields_when_decoding() {
        let line = r#"{"id":"x","source":"s","status":"running","future_field":1}"#;
        assert!(serde_json::from_str::<AgentEvent>(line).is_ok());
    }

    #[test]
    fn status_parse_and_priority() {
        assert_eq!(AgentStatus::parse("waiting"), AgentStatus::Waiting);
        assert_eq!(AgentStatus::parse("nonsense"), AgentStatus::Running);
        assert!(AgentStatus::Waiting.priority() < AgentStatus::Error.priority());
        assert!(AgentStatus::Error.priority() < AgentStatus::Running.priority());
        assert!(AgentStatus::Running.priority() < AgentStatus::Done.priority());
        assert!(AgentStatus::Done.priority() < AgentStatus::Idle.priority());
    }
}

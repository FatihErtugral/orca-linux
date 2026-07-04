use crate::protocol::AgentEvent;
use crate::terminal::{self, TerminalIdentity};
use crate::transcript;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub type TitleProvider = Box<dyn Fn(&str) -> Option<String>>;

/// Builds `AgentEvent`s from parsed flags and optional hook payloads.
/// Collaborators are injected so the resolution logic can be tested without a
/// real terminal, clock, or transcript file.
pub struct EventBuilder {
    pub identity: Box<dyn Fn() -> TerminalIdentity>,
    pub originator_pid: Box<dyn Fn() -> i32>,
    pub session_title: TitleProvider,
    pub now: Box<dyn Fn() -> f64>,
    pub current_dir: Box<dyn Fn() -> String>,
}

impl EventBuilder {
    pub fn system() -> Self {
        Self {
            identity: Box::new(terminal::resolve),
            originator_pid: Box::new(terminal::originator_pid),
            session_title: Box::new(transcript::session_title),
            now: Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0)
            }),
            current_dir: Box::new(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "/".into())
            }),
        }
    }

    pub fn event(&self, flags: &HashMap<String, String>, hook: Option<&Value>) -> AgentEvent {
        let source = flags
            .get("source")
            .cloned()
            .unwrap_or_else(|| "custom".into());
        let cwd = flags
            .get("cwd")
            .cloned()
            .or_else(|| hook_str(hook, "cwd"))
            .unwrap_or_else(|| (self.current_dir)());
        let id = flags
            .get("id")
            .cloned()
            .or_else(|| hook_str(hook, "session_id"))
            .unwrap_or_else(|| cwd.clone());
        let transcript_path = flags
            .get("transcript")
            .cloned()
            .or_else(|| hook_str(hook, "transcript_path"));
        let session_title = if source == "claude-code" {
            transcript_path
                .as_deref()
                .and_then(|p| (self.session_title)(p))
        } else {
            None
        };
        let title = flags
            .get("title")
            .cloned()
            .or(session_title)
            .unwrap_or_else(|| basename(&cwd));

        let mut status = flags
            .get("status")
            .cloned()
            .unwrap_or_else(|| "running".into());
        let mut message = flags.get("message").cloned();
        // A Stop that fires while background tasks are still pending is not
        // "your turn" — Claude will auto-resume, so keep the agent running.
        if status == "waiting" && background_work_pending(hook) {
            status = "running".into();
            message = Some("Working in background".into());
        }

        self.decorate(AgentEvent {
            id,
            source,
            title: Some(title),
            cwd: Some(cwd),
            status,
            message,
            transcript_path,
            permission_mode: hook_str(hook, "permission_mode"),
            ..Default::default()
        })
    }

    pub fn wrap_event(
        &self,
        id: &str,
        source: &str,
        title: &str,
        cwd: &str,
        status: &str,
        message: Option<String>,
    ) -> AgentEvent {
        self.decorate(AgentEvent {
            id: id.into(),
            source: source.into(),
            title: Some(title.into()),
            cwd: Some(cwd.into()),
            status: status.into(),
            message,
            ..Default::default()
        })
    }

    fn decorate(&self, base: AgentEvent) -> AgentEvent {
        let mut event = base;
        let terminal = (self.identity)();
        event.ts = Some((self.now)());
        event.tty = terminal.tty;
        event.term_program = terminal.term_program;
        event.session = terminal.session;
        event.app_bundle_id = terminal.app_bundle_id;
        event.pid = Some((self.originator_pid)());
        event
    }
}

pub fn background_work_pending(hook: Option<&Value>) -> bool {
    let Some(hook) = hook else { return false };
    let pending = hook
        .get("background_tasks_pending")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let awaiting = hook
        .get("awaiting_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    pending || awaiting
}

pub fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn hook_str(hook: Option<&Value>, key: &str) -> Option<String> {
    hook?.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder(identity: TerminalIdentity, title: Option<&str>, cwd: &str) -> EventBuilder {
        let title = title.map(str::to_string);
        let cwd = cwd.to_string();
        EventBuilder {
            identity: Box::new(move || identity.clone()),
            originator_pid: Box::new(|| 777),
            session_title: Box::new(move |_| title.clone()),
            now: Box::new(|| 123.0),
            current_dir: Box::new(move || cwd.clone()),
        }
    }

    fn flags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn claude_uses_transcript_title_and_terminal_identity() {
        let identity = TerminalIdentity {
            tty: Some("/dev/pts/1".into()),
            term_program: Some("konsole".into()),
            session: None,
            app_bundle_id: None,
        };
        let event = builder(identity, Some("My Session"), "/cwd").event(
            &flags(&[
                ("source", "claude-code"),
                ("status", "waiting"),
                ("cwd", "/a/b"),
                ("transcript", "/x"),
            ]),
            None,
        );
        assert_eq!(event.title.as_deref(), Some("My Session"));
        assert_eq!(event.status, "waiting");
        assert_eq!(event.tty.as_deref(), Some("/dev/pts/1"));
        assert_eq!(event.term_program.as_deref(), Some("konsole"));
        assert_eq!(event.ts, Some(123.0));
        assert_eq!(event.pid, Some(777));
    }

    #[test]
    fn hook_provides_id_cwd_and_basename_title() {
        let hook =
            serde_json::json!({"session_id": "abc", "cwd": "/proj", "transcript_path": "/t"});
        let event = builder(TerminalIdentity::default(), None, "/cwd")
            .event(&flags(&[("source", "claude-code")]), Some(&hook));
        assert_eq!(event.id, "abc");
        assert_eq!(event.cwd.as_deref(), Some("/proj"));
        assert_eq!(event.title.as_deref(), Some("proj"));
    }

    #[test]
    fn non_claude_source_ignores_transcript_title() {
        let event = builder(
            TerminalIdentity::default(),
            Some("should-not-use"),
            "/work/dir",
        )
        .event(&flags(&[("source", "custom"), ("id", "x")]), None);
        assert_eq!(event.id, "x");
        assert_eq!(event.title.as_deref(), Some("dir"));
    }

    #[test]
    fn explicit_title_and_status_defaults() {
        let event = builder(TerminalIdentity::default(), None, "/cwd")
            .event(&flags(&[("title", "Explicit"), ("id", "z")]), None);
        assert_eq!(event.title.as_deref(), Some("Explicit"));
        assert_eq!(event.source, "custom");
        assert_eq!(event.status, "running");
    }

    #[test]
    fn stop_with_pending_background_stays_running() {
        let hook = serde_json::json!({
            "session_id": "s", "cwd": "/p",
            "hook_event_name": "Stop", "background_tasks_pending": true
        });
        let event = builder(TerminalIdentity::default(), None, "/cwd").event(
            &flags(&[("source", "claude-code"), ("status", "waiting")]),
            Some(&hook),
        );
        assert_eq!(event.status, "running");
        assert_eq!(event.message.as_deref(), Some("Working in background"));
    }

    #[test]
    fn stop_awaiting_background_stays_running() {
        let hook = serde_json::json!({"session_id": "s", "awaiting_background": true});
        let event = builder(TerminalIdentity::default(), None, "/cwd").event(
            &flags(&[("source", "claude-code"), ("status", "waiting")]),
            Some(&hook),
        );
        assert_eq!(event.status, "running");
    }

    #[test]
    fn stop_without_background_becomes_waiting() {
        let hook = serde_json::json!({
            "session_id": "s",
            "background_tasks_pending": false, "awaiting_background": false
        });
        let event = builder(TerminalIdentity::default(), None, "/cwd").event(
            &flags(&[
                ("source", "claude-code"),
                ("status", "waiting"),
                ("message", "Your turn"),
            ]),
            Some(&hook),
        );
        assert_eq!(event.status, "waiting");
        assert_eq!(event.message.as_deref(), Some("Your turn"));
    }

    #[test]
    fn event_carries_transcript_path() {
        let hook = serde_json::json!({"session_id": "s", "transcript_path": "/t/x.jsonl"});
        let event = builder(TerminalIdentity::default(), Some("T"), "/cwd").event(
            &flags(&[("source", "claude-code"), ("status", "running")]),
            Some(&hook),
        );
        assert_eq!(event.transcript_path.as_deref(), Some("/t/x.jsonl"));
    }
}

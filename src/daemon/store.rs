use crate::protocol::{AgentEvent, AgentStatus};
use std::collections::{HashMap, HashSet};

/// In-memory model of one tracked agent session. Times are epoch seconds so
/// they compare directly with the wire `ts` field.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub cwd: Option<String>,
    pub status: AgentStatus,
    pub message: Option<String>,
    pub last_update: f64,
    /// Start of the current running period (None when not running).
    pub run_started_at: Option<f64>,
    /// Duration of the most recently completed running period.
    pub last_run_duration: f64,
    pub tty: Option<String>,
    pub term_program: Option<String>,
    pub session: Option<String>,
    pub app_bundle_id: Option<String>,
    pub transcript_path: Option<String>,
    pub pid: Option<i32>,
}

impl Agent {
    /// Whether the agent is running or waiting (i.e. an open, live session).
    pub fn is_active(&self) -> bool {
        matches!(self.status, AgentStatus::Running | AgentStatus::Waiting)
    }

    /// Ticks only while running; otherwise the last completed run's duration.
    pub fn duration(&self, now: f64) -> f64 {
        if self.status == AgentStatus::Running {
            if let Some(start) = self.run_started_at {
                return (now - start).max(0.0);
            }
        }
        self.last_run_duration
    }
}

/// A state transition the daemon should surface as a desktop notification.
/// Preference gating happens at the call site, keeping the store pure.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub status: AgentStatus,
    pub title: String,
    pub message: Option<String>,
}

/// Central state machine — a pure port of the macOS AgentStore. No I/O, no
/// clock: `now` is passed in and liveness is an injected predicate.
pub struct AgentStore {
    map: HashMap<String, Agent>,
    ollama_ids: HashSet<String>,
    done_grace: f64,
    stale_ttl: f64,
}

impl Default for AgentStore {
    fn default() -> Self {
        Self::new(90.0, 1800.0)
    }
}

impl AgentStore {
    pub fn new(done_grace: f64, stale_ttl: f64) -> Self {
        Self {
            map: HashMap::new(),
            ollama_ids: HashSet::new(),
            done_grace,
            stale_ttl,
        }
    }

    pub fn is_process_alive(pid: i32) -> bool {
        (unsafe { libc::kill(pid, 0) } == 0)
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Count of agents actively working (left number in `2 active · 3 open`).
    pub fn running_count(&self) -> usize {
        self.map
            .values()
            .filter(|a| a.status == AgentStatus::Running)
            .count()
    }

    /// Count of open sessions — everything except finished/idle (right number).
    pub fn open_session_count(&self) -> usize {
        self.map
            .values()
            .filter(|a| !matches!(a.status, AgentStatus::Done | AgentStatus::Idle))
            .count()
    }

    /// Agents sorted for display: attention first, then most recently updated.
    pub fn agents(&self) -> Vec<&Agent> {
        let mut agents: Vec<&Agent> = self.map.values().collect();
        agents.sort_by(|a, b| {
            a.status.priority().cmp(&b.status.priority()).then(
                b.last_update
                    .partial_cmp(&a.last_update)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        agents
    }

    pub fn get(&self, id: &str) -> Option<&Agent> {
        self.map.get(id)
    }

    /// Removes agents whose owning process died. Status itself is exactly what
    /// the hooks report — no inference. Returns true if anything changed.
    pub fn evaluate_health(&mut self, process_alive: &dyn Fn(i32) -> bool) -> bool {
        let before = self.map.len();
        self.map
            .retain(|_, agent| agent.pid.map(process_alive).unwrap_or(true));
        self.map.len() != before
    }

    pub fn apply(&mut self, event: &AgentEvent, now: f64) -> Option<Notification> {
        if event.status == "closed" || event.status == "ended" {
            self.map.remove(&event.id);
            return None;
        }

        let status = AgentStatus::parse(&event.status);
        let previous = self.map.get(&event.id).map(|a| a.status);

        let mut agent = self.map.remove(&event.id).unwrap_or_else(|| Agent {
            id: event.id.clone(),
            source: event.source.clone(),
            title: String::new(),
            cwd: event.cwd.clone(),
            status,
            message: event.message.clone(),
            last_update: now,
            run_started_at: None,
            last_run_duration: 0.0,
            tty: None,
            term_program: None,
            session: None,
            app_bundle_id: None,
            transcript_path: None,
            pid: None,
        });

        update_run_timing(&mut agent, previous, status, now);

        agent.source = event.source.clone();
        if let Some(cwd) = &event.cwd {
            agent.cwd = Some(cwd.clone());
        }
        match &event.title {
            Some(title) if !title.is_empty() => agent.title = title.clone(),
            _ if agent.title.is_empty() => agent.title = display_title(event),
            _ => {}
        }
        copy_if_some(&event.tty, &mut agent.tty);
        copy_if_some(&event.term_program, &mut agent.term_program);
        copy_if_some(&event.session, &mut agent.session);
        copy_if_some(&event.app_bundle_id, &mut agent.app_bundle_id);
        copy_if_some(&event.transcript_path, &mut agent.transcript_path);
        if let Some(pid) = event.pid {
            agent.pid = Some(pid);
        }
        agent.status = status;
        agent.message = event.message.clone();
        agent.last_update = now;

        let notification = if previous != Some(status) {
            notification_for(&agent)
        } else {
            None
        };
        self.map.insert(event.id.clone(), agent);
        notification
    }

    /// Update a title out-of-band (e.g. after a /rename is detected in the
    /// transcript). Returns true if the title actually changed.
    pub fn update_title(&mut self, id: &str, title: &str) -> bool {
        if title.is_empty() {
            return false;
        }
        match self.map.get_mut(id) {
            Some(agent) if agent.title != title => {
                agent.title = title.to_string();
                true
            }
            _ => false,
        }
    }

    #[allow(dead_code)] // post-MVP seam (ollama polling)
    pub fn sync_ollama(&mut self, models: &[String], now: f64) {
        let current: HashSet<String> = models.iter().map(|m| format!("ollama:{m}")).collect();
        for name in models {
            let id = format!("ollama:{name}");
            let agent = self.map.entry(id.clone()).or_insert_with(|| Agent {
                id,
                source: "ollama".into(),
                title: name.clone(),
                cwd: None,
                status: AgentStatus::Running,
                message: None,
                last_update: now,
                run_started_at: None,
                last_run_duration: 0.0,
                tty: None,
                term_program: None,
                session: None,
                app_bundle_id: None,
                transcript_path: None,
                pid: None,
            });
            if agent.run_started_at.is_none() {
                agent.run_started_at = Some(now);
            }
            agent.status = AgentStatus::Running;
            agent.last_update = now;
        }
        let stale: Vec<String> = self.ollama_ids.difference(&current).cloned().collect();
        for id in stale {
            self.map.remove(&id);
        }
        self.ollama_ids = current;
    }

    /// Manually dismiss a single agent (e.g. a waiting session you're done with).
    pub fn remove(&mut self, id: &str) {
        self.map.remove(id);
    }

    pub fn prune(&mut self, now: f64) {
        let done_grace = self.done_grace;
        let stale_ttl = self.stale_ttl;
        self.map.retain(|_, agent| {
            let age = now - agent.last_update;
            if matches!(agent.status, AgentStatus::Done | AgentStatus::Idle) {
                return age < done_grace;
            }
            // Agents with a pid are governed by process liveness
            // (evaluate_health), not by an arbitrary age cutoff — an
            // idle-but-open session stays.
            if agent.pid.is_some() {
                return true;
            }
            age < stale_ttl
        });
    }
}

fn update_run_timing(
    agent: &mut Agent,
    previous: Option<AgentStatus>,
    next: AgentStatus,
    now: f64,
) {
    let was_running = previous == Some(AgentStatus::Running);
    let will_run = next == AgentStatus::Running;
    if will_run && !was_running {
        agent.run_started_at = Some(now);
    } else if !will_run && was_running {
        if let Some(start) = agent.run_started_at {
            agent.last_run_duration = now - start;
        }
        agent.run_started_at = None;
    }
}

fn notification_for(agent: &Agent) -> Option<Notification> {
    match agent.status {
        AgentStatus::Waiting | AgentStatus::Done | AgentStatus::Error => Some(Notification {
            status: agent.status,
            title: agent.title.clone(),
            message: agent.message.clone(),
        }),
        AgentStatus::Running | AgentStatus::Idle => None,
    }
}

fn display_title(event: &AgentEvent) -> String {
    match &event.cwd {
        Some(cwd) if !cwd.is_empty() => crate::cli::event_builder::basename(cwd),
        _ => event.id.clone(),
    }
}

fn copy_if_some(source: &Option<String>, target: &mut Option<String>) {
    if let Some(value) = source {
        *target = Some(value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, status: &str) -> AgentEvent {
        event_with(id, status, "claude-code", "proj")
    }

    fn event_with(id: &str, status: &str, source: &str, title: &str) -> AgentEvent {
        AgentEvent {
            id: id.into(),
            source: source.into(),
            title: Some(title.into()),
            cwd: Some("/proj".into()),
            status: status.into(),
            ..Default::default()
        }
    }

    fn claude_event(status: &str, pid: Option<i32>) -> AgentEvent {
        AgentEvent {
            id: "s1".into(),
            source: "claude-code".into(),
            title: Some("proj".into()),
            cwd: Some("/p".into()),
            status: status.into(),
            transcript_path: Some("/t/x.jsonl".into()),
            pid,
            ..Default::default()
        }
    }

    #[test]
    fn running_then_waiting_freezes_duration() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "running"), 1000.0);
        assert_eq!(store.agents()[0].duration(1005.0), 5.0);

        store.apply(&event("a", "waiting"), 1005.0);
        assert_eq!(store.agents()[0].duration(1105.0), 5.0);
    }

    #[test]
    fn new_run_resets_duration() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "running"), 0.0);
        store.apply(&event("a", "waiting"), 10.0);
        store.apply(&event("a", "running"), 10.0);
        assert_eq!(store.agents()[0].duration(13.0), 3.0);
    }

    #[test]
    fn running_and_open_counts() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "running"), 0.0);
        store.apply(&event("b", "running"), 0.0);
        store.apply(&event("c", "waiting"), 0.0);
        assert_eq!(store.running_count(), 2);
        assert_eq!(store.open_session_count(), 3);

        store.apply(&event("b", "done"), 0.0);
        assert_eq!(store.running_count(), 1);
        assert_eq!(store.open_session_count(), 2);
    }

    #[test]
    fn closed_removes_agent() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "running"), 0.0);
        assert_eq!(store.agents().len(), 1);
        store.apply(&event("a", "closed"), 0.0);
        assert!(store.agents().is_empty());
    }

    #[test]
    fn remove_dismisses_agent() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "waiting"), 0.0);
        store.remove("a");
        assert!(store.agents().is_empty());
    }

    #[test]
    fn notifies_on_attention_transitions_only() {
        let mut store = AgentStore::default();
        assert!(store.apply(&event("a", "running"), 0.0).is_none());
        assert!(store.apply(&event("a", "waiting"), 1.0).is_some());
        assert!(store.apply(&event("a", "waiting"), 2.0).is_none());
        assert!(store.apply(&event("a", "error"), 3.0).is_some());
    }

    #[test]
    fn notification_carries_title_and_message() {
        let mut store = AgentStore::default();
        let mut waiting = event("a", "waiting");
        waiting.message = Some("Response ready".into());
        let notification = store.apply(&waiting, 0.0).unwrap();
        assert_eq!(notification.status, AgentStatus::Waiting);
        assert_eq!(notification.title, "proj");
        assert_eq!(notification.message.as_deref(), Some("Response ready"));
    }

    #[test]
    fn title_falls_back_to_cwd_basename() {
        let mut store = AgentStore::default();
        let event = AgentEvent {
            id: "a".into(),
            source: "custom".into(),
            title: Some(String::new()),
            cwd: Some("/Users/x/my-project".into()),
            status: "running".into(),
            ..Default::default()
        };
        store.apply(&event, 0.0);
        assert_eq!(store.agents()[0].title, "my-project");
    }

    #[test]
    fn prune_drops_finished_past_grace() {
        let mut store = AgentStore::new(90.0, 1800.0);
        store.apply(&event("a", "done"), 0.0);
        store.prune(120.0);
        assert!(store.agents().is_empty());
    }

    #[test]
    fn ollama_sync_adds_and_removes() {
        let mut store = AgentStore::default();
        store.sync_ollama(&["llama3".into(), "qwen".into()], 0.0);
        assert_eq!(store.running_count(), 2);
        store.sync_ollama(&["llama3".into()], 1.0);
        assert_eq!(store.agents().len(), 1);
        assert_eq!(store.agents()[0].title, "llama3");
    }

    #[test]
    fn dead_process_removes_agent() {
        let mut store = AgentStore::default();
        store.apply(&claude_event("running", Some(42)), 0.0);
        assert_eq!(store.agents().len(), 1);

        assert!(store.evaluate_health(&|_| false));
        assert!(store.agents().is_empty());
    }

    #[test]
    fn status_is_exactly_what_hooks_report() {
        let mut store = AgentStore::default();
        store.apply(&claude_event("waiting", Some(42)), 10_000.0);
        assert_eq!(store.agents()[0].status, AgentStatus::Waiting);

        // Long silence changes nothing — only hook events move state.
        assert!(!store.evaluate_health(&|pid| pid == 42));
        assert_eq!(store.agents()[0].status, AgentStatus::Waiting);

        store.apply(&claude_event("running", Some(42)), 13_600.0);
        assert_eq!(store.agents()[0].status, AgentStatus::Running);
    }

    #[test]
    fn live_pid_survives_stale_ttl_prune() {
        let mut store = AgentStore::default();
        store.apply(&claude_event("waiting", Some(42)), 10_000.0);
        store.prune(17_200.0);
        assert_eq!(store.agents().len(), 1);
    }

    #[test]
    fn pidless_agent_ages_out_after_stale_ttl() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "waiting"), 0.0);
        store.prune(1799.0);
        assert_eq!(store.agents().len(), 1);
        store.prune(1801.0);
        assert!(store.agents().is_empty());
    }

    #[test]
    fn sorts_by_priority_then_recency() {
        let mut store = AgentStore::default();
        store.apply(&event("running-old", "running"), 1.0);
        store.apply(&event("done", "done"), 5.0);
        store.apply(&event("waiting", "waiting"), 2.0);
        store.apply(&event("running-new", "running"), 3.0);
        let ids: Vec<&str> = store.agents().iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["waiting", "running-new", "running-old", "done"]);
    }

    #[test]
    fn update_title_only_changes_existing_nonempty() {
        let mut store = AgentStore::default();
        store.apply(&event("a", "running"), 0.0);
        assert!(store.update_title("a", "renamed"));
        assert!(!store.update_title("a", "renamed"));
        assert!(!store.update_title("a", ""));
        assert!(!store.update_title("missing", "x"));
        assert_eq!(store.agents()[0].title, "renamed");
    }

    #[test]
    fn merges_optionals_field_by_field() {
        let mut store = AgentStore::default();
        let mut first = event("a", "running");
        first.tty = Some("/dev/pts/1".into());
        first.pid = Some(7);
        store.apply(&first, 0.0);

        // A later event without tty/pid must not erase them.
        store.apply(&event("a", "waiting"), 1.0);
        let agent = store.agents()[0];
        assert_eq!(agent.tty.as_deref(), Some("/dev/pts/1"));
        assert_eq!(agent.pid, Some(7));
    }
}

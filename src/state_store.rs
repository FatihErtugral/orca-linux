use crate::protocol::AgentEvent;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Persists the last event of each open agent to disk so the daemon can
/// rediscover sessions that were already running when it launched. Only open
/// states (`running`, `waiting`) are kept; anything else removes the file.
pub struct StateStore {
    pub directory: PathBuf,
}

impl StateStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn default_store() -> Self {
        Self::new(crate::paths::state_dir())
    }

    /// Records the event if the agent is open, otherwise clears any stored file.
    pub fn record(&self, event: &AgentEvent) {
        match event.status.as_str() {
            "running" | "waiting" => self.save(event),
            _ => self.remove(&event.id),
        }
    }

    pub fn save(&self, event: &AgentEvent) {
        let _ = fs::create_dir_all(&self.directory);
        if let Ok(data) = serde_json::to_vec(event) {
            let _ = fs::write(self.file_path(&event.id), data);
        }
    }

    pub fn remove(&self, id: &str) {
        let _ = fs::remove_file(self.file_path(id));
    }

    /// Loads persisted open-session events. Events carrying a pid are kept
    /// exactly as long as that process is alive; `max_age` is only the fallback
    /// for events from sources that have no pid. Stale files are deleted.
    pub fn load_all(
        &self,
        max_age: f64,
        now_epoch: f64,
        process_alive: &dyn Fn(i32) -> bool,
    ) -> Vec<AgentEvent> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(event) = read_event(&path) else {
                continue;
            };
            let stale = match event.pid {
                Some(pid) => !process_alive(pid),
                None => event.ts.map(|ts| now_epoch - ts > max_age).unwrap_or(true),
            };
            if stale {
                let _ = fs::remove_file(&path);
                continue;
            }
            events.push(event);
        }
        events
    }

    fn file_path(&self, id: &str) -> PathBuf {
        let hash = Sha256::digest(id.as_bytes());
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        self.directory.join(hex + ".json")
    }
}

fn read_event(path: &Path) -> Option<AgentEvent> {
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, status: &str) -> AgentEvent {
        AgentEvent {
            id: id.into(),
            source: "custom".into(),
            status: status.into(),
            // Pid-less events with no ts count as stale on load (macOS parity),
            // so give tests a fresh timestamp by default.
            ts: Some(0.0),
            ..Default::default()
        }
    }

    fn store() -> (StateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (StateStore::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn records_open_states_and_clears_others() {
        let (store, _dir) = store();
        store.record(&event("a", "running"));
        assert_eq!(store.load_all(1800.0, 0.0, &|_| true).len(), 1);

        store.record(&event("a", "done"));
        assert!(store.load_all(1800.0, 0.0, &|_| true).is_empty());
    }

    #[test]
    fn load_drops_events_with_dead_pids_and_deletes_files() {
        let (store, _dir) = store();
        let mut with_pid = event("a", "running");
        with_pid.pid = Some(4242);
        store.save(&with_pid);

        assert!(store.load_all(1800.0, 0.0, &|_| false).is_empty());
        // The stale file was deleted, so a permissive load stays empty.
        assert!(store.load_all(1800.0, 0.0, &|_| true).is_empty());
    }

    #[test]
    fn load_keeps_events_with_live_pids_regardless_of_age() {
        let (store, _dir) = store();
        let mut with_pid = event("a", "running");
        with_pid.pid = Some(4242);
        with_pid.ts = Some(0.0); // ancient
        store.save(&with_pid);

        assert_eq!(store.load_all(1800.0, 1_000_000.0, &|_| true).len(), 1);
    }

    #[test]
    fn load_ages_out_pidless_events() {
        let (store, _dir) = store();
        let mut fresh = event("fresh", "waiting");
        fresh.ts = Some(900.0);
        let mut old = event("old", "waiting");
        old.ts = Some(0.0);
        let mut missing_ts = event("missing", "waiting");
        missing_ts.ts = None;
        store.save(&fresh);
        store.save(&old);
        store.save(&missing_ts);

        let loaded = store.load_all(1800.0, 2000.0, &|_| true);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "fresh");
    }

    #[test]
    fn same_id_overwrites_same_file() {
        let (store, _dir) = store();
        store.save(&event("a", "running"));
        store.save(&event("a", "waiting"));
        assert_eq!(store.load_all(1800.0, 0.0, &|_| true).len(), 1);
    }
}

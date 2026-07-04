use super::store::AgentStore;
use std::collections::HashMap;
use std::time::SystemTime;

/// Keeps Claude session titles fresh between hook events (e.g. after /rename),
/// by re-deriving them from each open session's transcript. Transcripts are
/// only re-parsed when their modification time changes.
pub struct TitleRefresher {
    title_provider: Box<dyn Fn(&str) -> Option<String>>,
    modification_time: Box<dyn Fn(&str) -> Option<SystemTime>>,
    last_seen: HashMap<String, SystemTime>,
}

impl TitleRefresher {
    pub fn system() -> Self {
        Self::new(
            Box::new(|path| crate::transcript::session_title(path)),
            Box::new(|path| std::fs::metadata(path).ok()?.modified().ok()),
        )
    }

    pub fn new(
        title_provider: Box<dyn Fn(&str) -> Option<String>>,
        modification_time: Box<dyn Fn(&str) -> Option<SystemTime>>,
    ) -> Self {
        Self {
            title_provider,
            modification_time,
            last_seen: HashMap::new(),
        }
    }

    /// Returns true if any title changed.
    pub fn refresh(&mut self, store: &mut AgentStore) -> bool {
        let candidates: Vec<(String, String)> = store
            .agents()
            .iter()
            .filter(|a| a.source == "claude-code" && a.is_active())
            .filter_map(|a| a.transcript_path.clone().map(|p| (a.id.clone(), p)))
            .collect();

        let mut changed = false;
        for (id, path) in candidates {
            let Some(modified) = (self.modification_time)(&path) else {
                continue;
            };
            if self.last_seen.get(&path).is_some_and(|seen| *seen >= modified) {
                continue;
            }
            self.last_seen.insert(path.clone(), modified);

            if let Some(title) = (self.title_provider)(&path) {
                changed |= store.update_title(&id, &title);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AgentEvent;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    fn store_with_claude_agent() -> AgentStore {
        let mut store = AgentStore::default();
        let event = AgentEvent {
            id: "s1".into(),
            source: "claude-code".into(),
            title: Some("old title".into()),
            status: "running".into(),
            transcript_path: Some("/t/x.jsonl".into()),
            ..Default::default()
        };
        store.apply(&event, 0.0);
        store
    }

    #[test]
    fn refreshes_title_when_transcript_changes() {
        let mut store = store_with_claude_agent();
        let mut refresher = TitleRefresher::new(
            Box::new(|_| Some("new title".into())),
            Box::new(|_| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10))),
        );
        assert!(refresher.refresh(&mut store));
        assert_eq!(store.agents()[0].title, "new title");
    }

    #[test]
    fn skips_unchanged_transcripts() {
        let mut store = store_with_claude_agent();
        let parse_count = Rc::new(Cell::new(0));
        let count = parse_count.clone();
        let mut refresher = TitleRefresher::new(
            Box::new(move |_| {
                count.set(count.get() + 1);
                Some("new title".into())
            }),
            Box::new(|_| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10))),
        );
        refresher.refresh(&mut store);
        refresher.refresh(&mut store);
        assert_eq!(parse_count.get(), 1);
    }

    #[test]
    fn ignores_non_claude_and_inactive_agents() {
        let mut store = AgentStore::default();
        let mut wrap = AgentEvent {
            id: "w".into(),
            source: "custom".into(),
            title: Some("wrap".into()),
            status: "running".into(),
            transcript_path: Some("/t/w.jsonl".into()),
            ..Default::default()
        };
        store.apply(&wrap, 0.0);
        wrap.id = "done-claude".into();
        wrap.source = "claude-code".into();
        wrap.status = "done".into();
        store.apply(&wrap, 0.0);

        let mut refresher = TitleRefresher::new(
            Box::new(|_| Some("should not apply".into())),
            Box::new(|_| Some(SystemTime::UNIX_EPOCH)),
        );
        assert!(!refresher.refresh(&mut store));
    }
}

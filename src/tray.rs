use crate::daemon::prefs::Config;
use crate::daemon::store::AgentStore;
use crate::daemon::Msg;
use std::sync::mpsc::Sender;

/// Placeholder until the ksni implementation lands (M6): the daemon runs
/// headless and reports why.
pub struct Handle;

impl Handle {
    pub fn update(&self, _store: &AgentStore, _config: &Config) {}
}

pub fn spawn(_tx: Sender<Msg>) -> Result<Handle, String> {
    Err("tray UI not implemented yet — use --no-tray".into())
}

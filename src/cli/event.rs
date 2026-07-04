use super::event_builder::EventBuilder;
use crate::protocol::AgentEvent;
use crate::state_store::StateStore;
use crate::{args, paths, socket};
use serde_json::Value;
use std::io::Read;

pub fn run_event(argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    let hook = read_hook_payload();
    let event = EventBuilder::system().event(&parsed.flags, hook.as_ref());
    deliver(&event);
    0
}

/// Send to the running daemon and persist the open-session state for launch
/// discovery. Always succeeds from the caller's perspective so hooks never
/// break the agent.
pub fn deliver(event: &AgentEvent) {
    socket::send(&paths::socket_path(), event);
    StateStore::default_store().record(event);
}

/// Claude Code hooks pipe a JSON payload on stdin. Poll briefly instead of
/// blocking so manual invocations with an open-but-silent stdin never hang.
fn read_hook_payload() -> Option<Value> {
    if unsafe { libc::isatty(0) } != 0 {
        return None;
    }
    let mut poll_fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll_fd, 1, 300) };
    if ready <= 0 || poll_fd.revents & libc::POLLIN == 0 {
        return None;
    }
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data).ok()?;
    if data.is_empty() {
        return None;
    }
    serde_json::from_slice(&data).ok()
}

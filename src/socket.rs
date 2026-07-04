use crate::protocol::AgentEvent;
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

/// Sends an event to the daemon. Fire-and-forget: if the daemon is not running
/// the connection fails and the caller is unaffected.
pub fn send(path: &Path, event: &AgentEvent) -> bool {
    let Ok(data) = serde_json::to_vec(event) else {
        return false;
    };
    send_line(path, data)
}

/// Sends a popup-to-daemon control message (same NDJSON transport).
pub fn send_action(path: &Path, action: &Action) -> bool {
    let Ok(data) = serde_json::to_vec(&WireControl {
        kind: "action".into(),
        action: Some(action.clone()),
    }) else {
        return false;
    };
    send_line(path, data)
}

fn send_line(path: &Path, mut data: Vec<u8>) -> bool {
    data.push(b'\n');
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    stream.write_all(&data).is_ok()
}

/// A subscriber the daemon pushes UiState documents to — the unix-socket
/// popup and the plasmoid's websocket implement this the same way.
pub trait StateSink: Send {
    /// Push one UiState JSON document; returning false drops the subscriber.
    fn send_state(&mut self, json: &str) -> bool;
}

struct UnixSink(UnixStream);

impl StateSink for UnixSink {
    fn send_state(&mut self, json: &str) -> bool {
        self.0
            .write_all(json.as_bytes())
            .and_then(|_| self.0.write_all(b"\n"))
            .is_ok()
    }
}

/// Everything a client connection can deliver to the daemon.
#[allow(clippy::large_enum_variant)] // message volume is tiny; boxing buys nothing
pub enum Inbound {
    /// A status event from hooks / `orca wrap` (the original wire protocol).
    Event(AgentEvent),
    /// A client asked to receive UiState pushes.
    Subscribe(Box<dyn StateSink>),
    /// A popup/plasmoid control message (dismiss / focus / set_pref / quit).
    Action(Action),
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
pub struct Action {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<bool>,
}

/// Control envelope: `{"type":"subscribe"}` or `{"type":"action",...}`.
/// Plain `AgentEvent` lines have no `type` key, so old clients keep working.
#[derive(serde::Serialize, Deserialize)]
struct WireControl {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    action: Option<Action>,
}

/// Listens for newline-delimited JSON and forwards decoded messages to `sink`.
/// Returns an error if another daemon already owns the socket.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    pub fn bind(path: &Path) -> std::io::Result<Server> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if path.exists() {
            // A connectable socket means a live daemon — refuse to steal it.
            if UnixStream::connect(path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "another orca daemon is already listening on {}",
                        path.display()
                    ),
                ));
            }
            let _ = std::fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)?;
        Ok(Server {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// Accept loop; run this on a dedicated thread. Each client gets its own
    /// thread so one slow writer cannot block the rest.
    pub fn run(&self, sink: Sender<Inbound>) {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sink = sink.clone();
            thread::spawn(move || handle_client(stream, &sink));
        }
    }

    #[allow(dead_code)]
    pub fn socket_path(&self) -> &Path {
        &self.path
    }
}

fn handle_client(stream: UnixStream, sink: &Sender<Inbound>) {
    let reader_stream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        // read_until also returns a trailing un-newlined chunk at EOF,
        // mirroring the macOS server's leftover-buffer handling.
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(inbound) = parse_line(&buffer, &stream) {
                    let keep_reading = !matches!(inbound, Inbound::Subscribe(_));
                    let _ = sink.send(inbound);
                    if !keep_reading {
                        // Hold the connection open for the daemon's pushes;
                        // returning would close our clone of the socket too
                        // early only if we dropped the read half. Keep
                        // blocking until the peer disconnects.
                        let mut drain = Vec::new();
                        while matches!(reader.read_until(b'\n', &mut drain), Ok(n) if n > 0) {
                            drain.clear();
                        }
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
}

fn parse_line(line: &[u8], stream: &UnixStream) -> Option<Inbound> {
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    match value.get("type").and_then(|t| t.as_str()) {
        Some("subscribe") => stream
            .try_clone()
            .ok()
            .map(|s| Inbound::Subscribe(Box::new(UnixSink(s)))),
        Some("action") => serde_json::from_value::<Action>(value)
            .ok()
            .map(Inbound::Action),
        _ => serde_json::from_value::<AgentEvent>(value)
            .ok()
            .map(Inbound::Event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn temp_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orca.sock");
        (dir, path)
    }

    fn event(id: &str) -> AgentEvent {
        AgentEvent {
            id: id.into(),
            source: "custom".into(),
            status: "running".into(),
            ..Default::default()
        }
    }

    #[test]
    fn send_returns_false_when_no_daemon() {
        let (_dir, path) = temp_socket();
        assert!(!send(&path, &event("x")));
    }

    #[test]
    fn bind_replaces_a_dead_socket_file() {
        let (_dir, path) = temp_socket();
        drop(Server::bind(&path).unwrap()); // leaves a stale socket file behind
        assert!(path.exists());
        assert!(Server::bind(&path).is_ok());
    }

    #[test]
    fn bind_refuses_when_daemon_is_alive() {
        let (_dir, path) = temp_socket();
        let server = Server::bind(&path).unwrap();
        let (sink, _events) = mpsc::channel();
        let path_clone = path.clone();
        thread::spawn(move || server.run(sink));
        thread::sleep(std::time::Duration::from_millis(50));
        assert!(Server::bind(&path_clone).is_err());
    }

    #[test]
    fn routes_events_actions_and_subscribes() {
        let (_dir, path) = temp_socket();
        let server = Server::bind(&path).unwrap();
        let (sink, inbox) = mpsc::channel();
        thread::spawn(move || server.run(sink));
        thread::sleep(std::time::Duration::from_millis(50));

        assert!(send(&path, &event("a")));
        assert!(send_action(
            &path,
            &Action {
                action: "dismiss".into(),
                id: Some("a".into()),
                key: None,
                value: None,
            }
        ));
        let mut subscriber = UnixStream::connect(&path).unwrap();
        subscriber.write_all(b"{\"type\":\"subscribe\"}\n").unwrap();

        let mut got_event = false;
        let mut got_action = false;
        let mut got_subscribe = false;
        for _ in 0..3 {
            match inbox
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
            {
                Inbound::Event(e) => {
                    assert_eq!(e.id, "a");
                    got_event = true;
                }
                Inbound::Action(a) => {
                    assert_eq!(a.action, "dismiss");
                    assert_eq!(a.id.as_deref(), Some("a"));
                    got_action = true;
                }
                Inbound::Subscribe(mut sink) => {
                    // The daemon can push documents back on the subscription.
                    assert!(sink.send_state("{\"hello\":1}"));
                    got_subscribe = true;
                }
            }
        }
        assert!(got_event && got_action && got_subscribe);

        // The subscriber actually receives what the daemon writes.
        let mut reader = BufReader::new(subscriber);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "{\"hello\":1}");
    }
}

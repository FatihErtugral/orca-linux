use crate::protocol::AgentEvent;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

/// Sends an event to the daemon. Fire-and-forget: if the daemon is not running
/// the connection fails and the caller is unaffected.
pub fn send(path: &Path, event: &AgentEvent) -> bool {
    let Ok(mut data) = serde_json::to_vec(event) else {
        return false;
    };
    data.push(b'\n');
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    stream.write_all(&data).is_ok()
}

/// Listens for newline-delimited `AgentEvent` JSON and forwards decoded events
/// to `sink`. Returns an error if another daemon already owns the socket.
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
                    format!("another orca daemon is already listening on {}", path.display()),
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
    pub fn run(&self, sink: Sender<AgentEvent>) {
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

fn handle_client(stream: UnixStream, sink: &Sender<AgentEvent>) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        // read_until also returns a trailing un-newlined chunk at EOF,
        // mirroring the macOS server's leftover-buffer handling.
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                if let Ok(event) = serde_json::from_slice::<AgentEvent>(&buffer) {
                    let _ = sink.send(event);
                }
            }
            Err(_) => break,
        }
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

    #[test]
    fn send_returns_false_when_no_daemon() {
        let (_dir, path) = temp_socket();
        let event = AgentEvent {
            id: "x".into(),
            source: "custom".into(),
            status: "running".into(),
            ..Default::default()
        };
        assert!(!send(&path, &event));
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
        // Give the accept loop a beat to start.
        thread::sleep(std::time::Duration::from_millis(50));
        assert!(Server::bind(&path_clone).is_err());
    }
}

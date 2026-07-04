//! Minimal WebSocket server for the Plasma plasmoid. QML has no unix-socket
//! API, but ships QtWebSockets — so the daemon exposes the same subscribe/
//! action protocol over a loopback WebSocket. Std-only: the handshake is a
//! single HTTP 101 exchange (RFC 6455) and the framing below covers exactly
//! what Qt sends (single unfragmented text frames, ping, close).

use crate::socket::{Action, Inbound, StateSink};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

/// Fixed loopback range; the plasmoid tries these ports in order. A range
/// (not one port) so a second session/user on the machine still binds.
pub const PORTS: std::ops::RangeInclusive<u16> = 41957..=41966;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Binds the first free port in the range and serves connections on a
/// background thread. Returns the bound port.
pub fn spawn(sink: Sender<Inbound>) -> Option<u16> {
    for port in PORTS {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let sink = sink.clone();
                    thread::spawn(move || handle_connection(stream, &sink));
                }
            });
            return Some(port);
        }
    }
    None
}

struct WsSink {
    writer: Arc<Mutex<TcpStream>>,
}

impl StateSink for WsSink {
    fn send_state(&mut self, json: &str) -> bool {
        let Ok(mut stream) = self.writer.lock() else {
            return false;
        };
        write_text_frame(&mut stream, json).is_ok()
    }
}

fn handle_connection(mut stream: TcpStream, sink: &Sender<Inbound>) {
    if handshake(&mut stream).is_none() {
        return;
    }
    let writer = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    }));

    // A plasmoid connection is a subscription from the first byte on.
    let _ = sink.send(Inbound::Subscribe(Box::new(WsSink {
        writer: writer.clone(),
    })));

    while let Some((opcode, payload)) = read_frame(&mut stream) {
        match opcode {
            0x1 => {
                // Text: same control envelope as the unix socket.
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) {
                    if value.get("type").and_then(|t| t.as_str()) == Some("action") {
                        if let Ok(action) = serde_json::from_value::<Action>(value) {
                            let _ = sink.send(Inbound::Action(action));
                        }
                    }
                }
            }
            0x9 => {
                // Ping → pong with the same payload.
                if let Ok(mut writer) = writer.lock() {
                    let _ = write_frame(&mut writer, 0xA, &payload);
                }
            }
            0x8 => break, // close
            _ => {}
        }
    }
}

/// Reads the HTTP upgrade request and answers the 101 switch. Returns None on
/// anything that is not a well-formed websocket handshake.
fn handshake(stream: &mut TcpStream) -> Option<()> {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        if request.len() > 8192 || stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request);
    let key = text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("sec-websocket-key")
            .then(|| value.trim().to_string())
    })?;

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    stream.write_all(response.as_bytes()).ok()
}

fn accept_key(key: &str) -> String {
    let digest = Sha1::digest(format!("{key}{GUID}").as_bytes());
    base64(&digest)
}

fn write_text_frame(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    write_frame(stream, 0x1, text.as_bytes())
}

/// Server→client frame: FIN set, never masked.
fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        len if len < 126 => frame.push(len as u8),
        len if len < 65536 => {
            frame.push(126);
            frame.extend((len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend((len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

/// Client→server frame: always masked per RFC 6455. Returns opcode + payload.
fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).ok()?;
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    let mut len = (header[1] & 0x7F) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext).ok()?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext).ok()?;
        len = u64::from_be_bytes(ext);
    }
    if len > 1_048_576 {
        return None; // nothing legitimate is this large
    }
    let mask = if masked {
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask).ok()?;
        Some(mask)
    } else {
        None
    };
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).ok()?;
    if let Some(mask) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Some((opcode, payload))
}

/// Standard base64 with padding — 20 lines beat a dependency for one call.
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn accept_key_matches_rfc6455_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn full_roundtrip_subscribe_state_and_action() {
        let (sink, inbox) = mpsc::channel();
        let port = spawn(sink).expect("a free port in the range");

        // Client side: handshake…
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(
                b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            )
            .unwrap();
        let mut response = [0u8; 256];
        let n = client.read(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response[..n]);
        assert!(response.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));

        // …the server registers a subscriber sink the daemon can push through…
        let Ok(Inbound::Subscribe(mut subscriber)) = inbox.recv() else {
            panic!("expected subscription");
        };
        assert!(subscriber.send_state("{\"open\":1}"));
        let mut frame = [0u8; 64];
        let n = client.read(&mut frame).unwrap();
        assert_eq!(frame[0], 0x81);
        assert_eq!(&frame[2..n], b"{\"open\":1}");

        // …and masked client text frames arrive as actions.
        let payload = br#"{"type":"action","action":"dismiss","id":"a"}"#;
        let mask = [1u8, 2, 3, 4];
        let mut msg = vec![0x81, 0x80 | payload.len() as u8];
        msg.extend(mask);
        msg.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        client.write_all(&msg).unwrap();
        let Ok(Inbound::Action(action)) = inbox.recv() else {
            panic!("expected action");
        };
        assert_eq!(action.action, "dismiss");
        assert_eq!(action.id.as_deref(), Some("a"));
    }
}

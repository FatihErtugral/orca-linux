use std::env;
use std::ffi::CStr;
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalIdentity {
    pub tty: Option<String>,
    pub term_program: Option<String>,
    pub session: Option<String>,
    pub app_bundle_id: Option<String>,
}

/// Resolves which terminal owns this process. Works from inside Claude Code
/// hooks (stdio are pipes) by falling back to the controlling tty recorded in
/// /proc/self/stat.
pub fn resolve() -> TerminalIdentity {
    TerminalIdentity {
        tty: controlling_tty(),
        term_program: term_program(),
        session: env_non_empty("KONSOLE_DBUS_SESSION").or_else(|| env_non_empty("TERM_SESSION_ID")),
        // No bundle ids on Linux; carry Konsole's per-instance DBus service
        // name instead (useful for tab-level focusing later).
        app_bundle_id: env_non_empty("KONSOLE_DBUS_SERVICE"),
    }
}

/// PID of the process that owns this session — the grandparent when invoked
/// from a hook (claude spawns `sh -c` which spawns us), otherwise the shell's
/// parent (the terminal). Used by the daemon for liveness checks.
pub fn originator_pid() -> i32 {
    let parent = unsafe { libc::getppid() };
    if let Some(grandparent) = stat_ppid(parent) {
        if grandparent > 1 {
            return grandparent;
        }
    }
    parent
}

/// Controlling terminal device (e.g. `/dev/pts/3`). Prefers /proc/self/stat's
/// tty_nr, which survives even when all stdio are pipes (the hook case), then
/// falls back to ttyname on any stdio fd that is a terminal.
fn controlling_tty() -> Option<String> {
    if let Some(tty) = stat_tty(unsafe { libc::getpid() }) {
        return Some(tty);
    }
    for fd in [0, 1, 2] {
        if unsafe { libc::isatty(fd) } != 0 {
            let name = unsafe { libc::ttyname(fd) };
            if !name.is_null() {
                let s = unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn term_program() -> Option<String> {
    if let Some(program) = env_non_empty("TERM_PROGRAM") {
        return Some(program);
    }
    // Terminals that don't set TERM_PROGRAM usually leak an identifying var.
    let hints = [
        ("KONSOLE_DBUS_SESSION", "konsole"),
        ("KITTY_WINDOW_ID", "kitty"),
        ("ALACRITTY_SOCKET", "alacritty"),
        ("ALACRITTY_WINDOW_ID", "alacritty"),
        ("WEZTERM_PANE", "wezterm"),
        ("GNOME_TERMINAL_SCREEN", "gnome-terminal"),
    ];
    for (var, name) in hints {
        if env_non_empty(var).is_some() {
            return Some(name.to_string());
        }
    }
    None
}

/// Fields of /proc/<pid>/stat after the `comm` column. `comm` may contain
/// spaces and parentheses, so split on the LAST `)` first.
fn stat_fields(pid: i32) -> Option<Vec<String>> {
    let content = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &content[content.rfind(')')? + 1..];
    Some(after_comm.split_whitespace().map(str::to_string).collect())
}

/// Parent PID: field 4 of stat, i.e. index 1 after comm (state ppid pgrp ...).
fn stat_ppid(pid: i32) -> Option<i32> {
    stat_fields(pid)?.get(1)?.parse().ok()
}

/// Controlling tty: field 7 of stat, index 4 after comm. Encoded as a device
/// number; Unix98 pty majors are 136-143.
fn stat_tty(pid: i32) -> Option<String> {
    let tty_nr: u64 = stat_fields(pid)?.get(4)?.parse().ok()?;
    decode_tty_nr(tty_nr)
}

fn decode_tty_nr(tty_nr: u64) -> Option<String> {
    if tty_nr == 0 {
        return None;
    }
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    match major {
        136..=143 => Some(format!("/dev/pts/{}", (major - 136) * 256 + minor)),
        4 => Some(format!("/dev/tty{minor}")),
        _ => None,
    }
}

fn env_non_empty(key: &str) -> Option<String> {
    match env::var(key) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_unix98_pty_device_numbers() {
        // major 136, minor 3 → /dev/pts/3
        assert_eq!(decode_tty_nr((136 << 8) | 3).as_deref(), Some("/dev/pts/3"));
        // major 137 continues the pts range at 256
        assert_eq!(
            decode_tty_nr((137 << 8) | 1).as_deref(),
            Some("/dev/pts/257")
        );
        // major 4 is a virtual console
        assert_eq!(decode_tty_nr((4 << 8) | 2).as_deref(), Some("/dev/tty2"));
        // no controlling terminal
        assert_eq!(decode_tty_nr(0), None);
        // unknown major
        assert_eq!(decode_tty_nr(99 << 8), None);
    }

    #[test]
    fn originator_pid_is_a_live_process() {
        let pid = originator_pid();
        assert!(pid > 0);
        assert!(
            unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        );
    }

    #[test]
    fn stat_ppid_of_self_matches_getppid() {
        let me = unsafe { libc::getpid() };
        assert_eq!(stat_ppid(me), Some(unsafe { libc::getppid() }));
    }
}

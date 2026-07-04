//! Click-to-focus: bring the terminal (or editor) that owns an agent session
//! to the front. The Linux counterpart of the macOS TerminalActivating.swift.
//!
//! Strategy order, mirroring the macOS activator chain:
//! 1. VS Code family terminals expose `TERM_PROGRAM=vscode` — reopen the
//!    workspace folder via its URL scheme, which focuses the right window.
//! 2. Anything else: activate the window whose process is an ancestor of the
//!    agent's pid, via a throwaway KWin script (works on Wayland, where
//!    ordinary clients cannot raise windows themselves).

use std::fs;
use std::process::Command;

pub struct FocusTarget {
    pub pid: Option<i32>,
    pub cwd: Option<String>,
    pub term_program: Option<String>,
}

pub fn focus(target: &FocusTarget) {
    if let Some(program) = target.term_program.as_deref() {
        if program.eq_ignore_ascii_case("vscode") {
            if let Some(cwd) = &target.cwd {
                let _ = Command::new("xdg-open")
                    .arg(format!("vscode://file{cwd}"))
                    .spawn();
                return;
            }
        }
    }
    if let Some(pid) = target.pid {
        let chain = ancestor_chain(pid);
        if !kwin_activate(&chain) {
            log::debug!("focus: no KWin window matched pid chain {chain:?}");
        }
    }
}

/// The agent pid and all its ancestors (deepest first, init excluded). One of
/// them owns the terminal window.
fn ancestor_chain(pid: i32) -> Vec<i32> {
    let mut chain = Vec::new();
    let mut current = pid;
    while current > 1 && chain.len() < 32 {
        chain.push(current);
        let Some(parent) = stat_ppid(current) else {
            break;
        };
        current = parent;
    }
    chain
}

fn stat_ppid(pid: i32) -> Option<i32> {
    let content = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &content[content.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

/// Activate the first window owned by any pid in the chain, via KWin's
/// scripting DBus interface (the same trick kdotool uses). Covers both the
/// KWin 5 and 6 script APIs and object paths.
fn kwin_activate(pids: &[i32]) -> bool {
    if pids.is_empty() {
        return false;
    }
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        r#"
var pids = [{pid_list}];
var wins = workspace.windowList ? workspace.windowList() : workspace.clientList;
var found = null;
for (var i = 0; i < pids.length && !found; i++) {{
    for (var j = 0; j < wins.length; j++) {{
        if (wins[j].pid === pids[i]) {{ found = wins[j]; break; }}
    }}
}}
if (found) {{
    if (workspace.activeWindow !== undefined) {{ workspace.activeWindow = found; }}
    else {{ workspace.activeClient = found; }}
}}
"#
    );
    run_script_text("focus", &script)
}

/// Move our own popup window to the bottom-right of the work area, docked
/// above the panel like a Plasma applet. Wayland clients cannot position
/// their own windows, but a KWin script can.
pub fn position_popup_bottom_right() {
    let pid = std::process::id();
    let script = format!(
        r#"
var wins = workspace.windowList ? workspace.windowList() : workspace.clientList;
for (var i = 0; i < wins.length; i++) {{
    var w = wins[i];
    if (w.pid === {pid} && w.normalWindow) {{
        var area = workspace.clientArea(KWin.PlacementArea, w);
        var g = w.frameGeometry;
        w.frameGeometry = {{
            x: area.x + area.width - g.width - 8,
            y: area.y + area.height - g.height - 8,
            width: g.width,
            height: g.height
        }};
        break;
    }}
}}
"#
    );
    run_script_text("position", &script);
}

fn run_script_text(kind: &str, script: &str) -> bool {
    let path = std::env::temp_dir().join(format!("orca-{kind}-{}.js", std::process::id()));
    if fs::write(&path, script).is_err() {
        return false;
    }
    let result = run_kwin_script(&path);
    let _ = fs::remove_file(&path);
    result
}

fn run_kwin_script(path: &std::path::Path) -> bool {
    let output = Command::new("busctl")
        .args(["--user", "call", "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "loadScript", "s"])
        .arg(path)
        .output();
    let Ok(output) = output else { return false };
    if !output.status.success() {
        return false;
    }
    // Reply looks like: "i 12"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(id) = stdout.split_whitespace().nth(1).map(str::to_string) else {
        return false;
    };

    let mut ran = false;
    for object_path in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        let status = Command::new("busctl")
            .args(["--user", "call", "org.kde.KWin", &object_path, "org.kde.kwin.Script", "run"])
            .output();
        if status.map(|o| o.status.success()).unwrap_or(false) {
            ran = true;
            let _ = Command::new("busctl")
                .args(["--user", "call", "org.kde.KWin", &object_path, "org.kde.kwin.Script", "stop"])
                .output();
            break;
        }
    }
    ran
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_chain_starts_with_self_and_walks_up() {
        let me = std::process::id() as i32;
        let chain = ancestor_chain(me);
        assert_eq!(chain.first(), Some(&me));
        assert!(chain.len() >= 2); // at least us + a parent
        assert!(!chain.contains(&1));
    }

    #[test]
    fn ancestor_chain_of_unknown_pid_is_just_that_pid() {
        // A pid that (almost certainly) does not exist: stat read fails,
        // so the chain stops after the first entry.
        let chain = ancestor_chain(4_000_000);
        assert_eq!(chain, vec![4_000_000]);
    }
}

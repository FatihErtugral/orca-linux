use crate::{args, version};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Self-update from GitHub releases. Uses plain `curl` + `tar` so there is no
/// dependency on `gh` or an HTTP crate; release assets are public.
pub fn run_update(argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    let check_only = parsed.flags.contains_key("check");
    let current = parsed
        .flags
        .get("current")
        .cloned()
        .unwrap_or_else(|| version::CURRENT.to_string());

    let Some(tag) = latest_release_tag() else {
        eprintln!("orca: could not reach GitHub (api.github.com)");
        return 1;
    };

    if !version::is_newer(&tag, &current) {
        println!("orca is up to date (v{current})");
        return 0;
    }
    if check_only {
        println!("update-available {tag}");
        return 0;
    }

    println!("==> Updating v{current} -> {tag}");
    let tmp = std::env::temp_dir().join(format!("orca-update-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let result = install_release(&tag, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    match result {
        Ok(()) => {
            println!("==> Updated to {tag}");
            0
        }
        Err(message) => {
            eprintln!("orca: {message}");
            1
        }
    }
}

fn install_release(tag: &str, tmp: &Path) -> Result<(), String> {
    let asset = format!("orca-linux-{}.tar.gz", std::env::consts::ARCH);
    let url = format!(
        "https://github.com/{}/releases/download/{tag}/{asset}",
        version::REPO
    );
    println!("==> Downloading {asset}");
    let archive = tmp.join(&asset);
    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl not available: {e}"))?;
    if !status.success() {
        return Err(format!("download failed: {url}"));
    }

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(tmp)
        .status()
        .map_err(|e| format!("tar not available: {e}"))?;
    if !status.success() {
        return Err("extract failed".into());
    }

    let target = install_target();
    println!("==> Installing {}", target.display());
    // Unlink first: overwriting a running executable in place is unsafe.
    let _ = std::fs::remove_file(&target);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(tmp.join("orca"), &target).map_err(|e| format!("could not install: {e}"))?;
    let _ = Command::new("chmod").arg("+x").arg(&target).status();

    // Refresh the hook definitions with the NEW binary so hook-set changes
    // reach settings.json, not just the binary.
    println!("==> Refreshing Claude Code hooks");
    let _ = Command::new(&target).arg("install-hooks").status();

    restart_daemon(&target);
    Ok(())
}

/// The running executable's own path — updating in place keeps hooks, service
/// files and PATH entries valid.
fn install_target() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| crate::paths::home().join(".local/bin/orca"))
}

fn restart_daemon(target: &Path) {
    let managed = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "orca.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if managed {
        println!("==> Restarting orca.service");
        let _ = Command::new("systemctl")
            .args(["--user", "restart", "orca.service"])
            .status();
        return;
    }

    // Find daemon processes by exact name, excluding this updater itself
    // (both are named "orca", so a plain pkill would be self-inflicted).
    let daemons: Vec<i32> = Command::new("pgrep")
        .args(["-x", "orca"])
        .output()
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .filter(|pid| *pid != std::process::id() as i32)
                .collect()
        })
        .unwrap_or_default();
    for pid in &daemons {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    if !daemons.is_empty() {
        println!("==> Relaunching orca tray");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = Command::new(target)
            .arg("tray")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn latest_release_tag() -> Option<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            &format!(
                "https://api.github.com/repos/{}/releases/latest",
                version::REPO
            ),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let tag = body.get("tag_name")?.as_str()?;
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

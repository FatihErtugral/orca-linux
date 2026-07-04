use std::env;
use std::path::PathBuf;

/// XDG-flavored path resolution. Every location honors an `ORCA_*` override so
/// tests and a dev daemon can run beside a production one.
pub fn socket_path() -> PathBuf {
    if let Some(path) = env_path("ORCA_SOCKET") {
        return path;
    }
    if let Some(runtime) = env_path("XDG_RUNTIME_DIR") {
        return runtime.join("orca.sock");
    }
    PathBuf::from(format!("/tmp/orca-{}.sock", unsafe { libc::getuid() }))
}

pub fn state_dir() -> PathBuf {
    if let Some(path) = env_path("ORCA_STATE_DIR") {
        return path;
    }
    env_path("XDG_STATE_HOME")
        .unwrap_or_else(|| home().join(".local/state"))
        .join("orca/agents")
}

pub fn config_path() -> PathBuf {
    if let Some(path) = env_path("ORCA_CONFIG") {
        return path;
    }
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("orca/config.toml")
}

pub fn claude_settings_path() -> PathBuf {
    home().join(".claude/settings.json")
}

pub fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    match env::var(key) {
        Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

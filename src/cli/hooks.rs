use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;

/// Merges (and removes) Orca's Claude Code hooks in `settings.json` without
/// disturbing the user's other settings or hooks. The settings path and the
/// executable path are injectable so the merge can be tested against a temp file.
pub struct HookInstaller {
    pub settings_path: PathBuf,
    pub executable_path: String,
}

impl HookInstaller {
    pub fn system() -> Self {
        Self {
            settings_path: crate::paths::claude_settings_path(),
            executable_path: std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "orca".into()),
        }
    }

    pub fn hooks(&self) -> Vec<(&'static str, String)> {
        let exe = &self.executable_path;
        vec![
            ("SessionStart", format!("{exe} event --source claude-code --status running")),
            ("UserPromptSubmit", format!("{exe} event --source claude-code --status running")),
            ("Notification", format!("{exe} event --source claude-code --status waiting --message 'Waiting for your input'")),
            ("Stop", format!("{exe} event --source claude-code --status waiting --message 'Response ready, your turn'")),
            ("SessionEnd", format!("{exe} event --source claude-code --status closed")),
        ]
    }

    pub fn install(&self) -> bool {
        let mut root = self.load_settings();
        let mut hooks_map = take_object(&mut root, "hooks");
        for (event, command) in self.hooks() {
            let mut entries: Vec<Value> = hooks_map
                .remove(event)
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| !is_orca_entry(entry))
                .collect();
            entries.push(json!({"hooks": [{"type": "command", "command": command}]}));
            hooks_map.insert(event.to_string(), Value::Array(entries));
        }
        root.insert("hooks".into(), Value::Object(hooks_map));
        self.write_settings(&root)
    }

    pub fn uninstall(&self) -> bool {
        let mut root = self.load_settings();
        let Some(Value::Object(mut hooks_map)) = root.remove("hooks") else {
            return true;
        };
        for (event, _) in self.hooks() {
            let Some(Value::Array(entries)) = hooks_map.remove(event) else {
                continue;
            };
            let kept: Vec<Value> = entries.into_iter().filter(|e| !is_orca_entry(e)).collect();
            if !kept.is_empty() {
                hooks_map.insert(event.to_string(), Value::Array(kept));
            }
        }
        if !hooks_map.is_empty() {
            root.insert("hooks".into(), Value::Object(hooks_map));
        }
        self.write_settings(&root)
    }

    fn load_settings(&self) -> Map<String, Value> {
        let Ok(data) = fs::read(&self.settings_path) else {
            return Map::new();
        };
        match serde_json::from_slice::<Value>(&data) {
            Ok(Value::Object(map)) => map,
            _ => {
                eprintln!(
                    "orca: warning: {} is not valid JSON — starting from an empty settings object",
                    self.settings_path.display()
                );
                Map::new()
            }
        }
    }

    fn write_settings(&self, root: &Map<String, Value>) -> bool {
        let Ok(data) = serde_json::to_string_pretty(&Value::Object(root.clone())) else {
            return false;
        };
        if let Some(parent) = self.settings_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&self.settings_path, data + "\n").is_ok()
    }
}

fn take_object(root: &mut Map<String, Value>, key: &str) -> Map<String, Value> {
    match root.remove(key) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn is_orca_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|inner| {
            inner.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .map(|c| c.contains("orca event"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub fn run_install() -> i32 {
    let installer = HookInstaller::system();
    if !installer.install() {
        eprintln!("orca: failed to write {}", installer.settings_path.display());
        return 1;
    }
    println!("orca: hooks installed -> {}", installer.settings_path.display());
    println!("(Restart Claude Code or open /hooks for them to take effect.)");
    0
}

pub fn run_uninstall() -> i32 {
    let installer = HookInstaller::system();
    if !installer.uninstall() {
        eprintln!("orca: failed to write {}", installer.settings_path.display());
        return 1;
    }
    println!("orca: hooks removed");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_installer(initial: Option<Value>) -> (HookInstaller, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        if let Some(value) = initial {
            fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
        let installer = HookInstaller {
            settings_path: path,
            executable_path: "/usr/local/bin/orca".into(),
        };
        (installer, dir)
    }

    fn read_json(installer: &HookInstaller) -> Value {
        serde_json::from_slice(&fs::read(&installer.settings_path).unwrap()).unwrap()
    }

    fn orca_entries(root: &Value, event: &str) -> usize {
        root.get("hooks")
            .and_then(|h| h.get(event))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter(|e| is_orca_entry(e)).count())
            .unwrap_or(0)
    }

    #[test]
    fn install_adds_all_hooks_and_preserves_settings() {
        let (installer, _dir) = temp_installer(Some(json!({"theme": "dark"})));
        assert!(installer.install());

        let root = read_json(&installer);
        assert_eq!(root["theme"], "dark");
        for (event, _) in installer.hooks() {
            assert_eq!(orca_entries(&root, event), 1, "missing hook {event}");
        }
    }

    #[test]
    fn install_preserves_users_own_hook_on_same_event() {
        let (installer, _dir) = temp_installer(Some(json!({
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo mine"}]}]}
        })));
        assert!(installer.install());

        let root = read_json(&installer);
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn install_is_idempotent() {
        let (installer, _dir) = temp_installer(None);
        assert!(installer.install());
        assert!(installer.install());
        assert_eq!(orca_entries(&read_json(&installer), "Stop"), 1);
    }

    #[test]
    fn uninstall_removes_only_orca_hooks() {
        let (installer, _dir) = temp_installer(Some(json!({
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo mine"}]}]}
        })));
        assert!(installer.install());
        assert!(installer.uninstall());

        let root = read_json(&installer);
        assert_eq!(orca_entries(&root, "Stop"), 0);
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn uninstall_removes_empty_hooks_map_entirely() {
        let (installer, _dir) = temp_installer(None);
        assert!(installer.install());
        assert!(installer.uninstall());
        assert!(read_json(&installer).get("hooks").is_none());
    }

    #[test]
    fn corrupt_settings_are_replaced() {
        let (installer, _dir) = temp_installer(None);
        fs::write(&installer.settings_path, "{not json").unwrap();
        assert!(installer.install());
        assert_eq!(orca_entries(&read_json(&installer), "Stop"), 1);
    }
}

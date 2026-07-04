use crate::daemon::prefs::{Config, NotificationPreferences};
use crate::daemon::store::AgentStore;
use crate::daemon::{now_epoch, Msg, PrefKey};
use crate::protocol::AgentStatus;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Icon, Status, ToolTip};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

// Template dolphin art recolored by build.rs: light for normal, orange for
// attention. 96x96 ARGB32.
static ICON_NORMAL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orca-normal.argb"));
static ICON_ATTENTION: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/orca-attention.argb"));
const ICON_SIZE: i32 = 96;

/// Everything the tray renders, precomputed by the store loop. Snapshots are
/// compared before pushing so DBus only sees real changes.
#[derive(Clone, PartialEq, Default)]
struct Model {
    rows: Vec<AgentRow>,
    running: usize,
    open: usize,
    prefs: NotificationPreferences,
}

#[derive(Clone, PartialEq)]
struct AgentRow {
    id: String,
    title: String,
    source: String,
    status: AgentStatus,
    duration: String,
    message: Option<String>,
}

pub struct OrcaTray {
    model: Model,
    tx: Sender<Msg>,
}

impl OrcaTray {
    fn attention(&self) -> bool {
        self.model
            .rows
            .iter()
            .any(|r| matches!(r.status, AgentStatus::Waiting | AgentStatus::Error))
    }
}

impl ksni::Tray for OrcaTray {
    fn id(&self) -> String {
        "orca".into()
    }

    /// Left click toggles the popover, like the macOS status item. The DBus
    /// menu stays available on right click.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Msg::TogglePopup);
    }

    fn title(&self) -> String {
        "Orca".into()
    }

    fn status(&self) -> Status {
        // Never NeedsAttention: Plasma renders it as an endless pulsing
        // animation, which reads as urgency even for a routine "your turn".
        // The macOS design signals attention with a static color change, so
        // the orange icon variant (see icon_pixmap) carries that role alone.
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(if self.attention() {
            ICON_ATTENTION
        } else {
            ICON_NORMAL
        })]
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(ICON_ATTENTION)]
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Orca".into(),
            description: format!("{} active · {} open", self.model.running, self.model.open),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            MenuItem::Standard(StandardItem {
                label: format!("{} active · {} open", self.model.running, self.model.open),
                enabled: false,
                ..Default::default()
            }),
            MenuItem::Separator,
        ];

        if self.model.rows.is_empty() {
            items.push(MenuItem::Standard(StandardItem {
                label: "No active agents".into(),
                enabled: false,
                ..Default::default()
            }));
        }
        for row in &self.model.rows {
            items.push(agent_item(row));
        }

        items.push(MenuItem::Separator);
        items.push(settings_menu(&self.model.prefs));
        items.push(MenuItem::Separator);
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit Orca".into(),
            activate: Box::new(|tray: &mut OrcaTray| {
                let _ = tray.tx.send(Msg::Quit);
            }),
            ..Default::default()
        }));
        items
    }
}

fn agent_item(row: &AgentRow) -> MenuItem<OrcaTray> {
    let glyph = match row.status {
        AgentStatus::Running => "▶",
        AgentStatus::Waiting => "⏳",
        AgentStatus::Done => "✅",
        AgentStatus::Error => "❌",
        AgentStatus::Idle => "●",
    };
    let mut children: Vec<MenuItem<OrcaTray>> = vec![MenuItem::Standard(StandardItem {
        label: escape(&format!("{} · {}", row.source, row.status.label())),
        enabled: false,
        ..Default::default()
    })];
    if let Some(message) = &row.message {
        children.push(MenuItem::Standard(StandardItem {
            label: escape(message),
            enabled: false,
            ..Default::default()
        }));
    }
    children.push(MenuItem::Separator);
    let id = row.id.clone();
    children.push(MenuItem::Standard(StandardItem {
        label: "Dismiss".into(),
        activate: Box::new(move |tray: &mut OrcaTray| {
            let _ = tray.tx.send(Msg::Dismiss(id.clone()));
        }),
        ..Default::default()
    }));
    let focus_id = row.id.clone();
    children.push(MenuItem::Standard(StandardItem {
        label: "Focus terminal".into(),
        activate: Box::new(move |tray: &mut OrcaTray| {
            let _ = tray.tx.send(Msg::Focus(focus_id.clone()));
        }),
        ..Default::default()
    }));

    MenuItem::SubMenu(SubMenu {
        label: escape(&format!("{glyph} {} · {}", row.title, row.duration)),
        submenu: children,
        ..Default::default()
    })
}

fn settings_menu(prefs: &NotificationPreferences) -> MenuItem<OrcaTray> {
    let toggles: [(&str, PrefKey, bool, bool); 5] = [
        ("Notifications", PrefKey::Enabled, prefs.enabled, true),
        (
            "On waiting",
            PrefKey::OnWaiting,
            prefs.on_waiting,
            prefs.enabled,
        ),
        ("On done", PrefKey::OnDone, prefs.on_done, prefs.enabled),
        ("On error", PrefKey::OnError, prefs.on_error, prefs.enabled),
        ("Sound", PrefKey::Sound, prefs.sound, prefs.enabled),
    ];
    MenuItem::SubMenu(SubMenu {
        label: "Settings".into(),
        submenu: toggles
            .into_iter()
            .map(|(label, key, checked, enabled)| {
                MenuItem::Checkmark(CheckmarkItem {
                    label: label.into(),
                    checked,
                    enabled,
                    activate: Box::new(move |tray: &mut OrcaTray| {
                        let _ = tray.tx.send(Msg::SetPref(key, !checked));
                    }),
                    ..Default::default()
                })
            })
            .collect(),
        ..Default::default()
    })
}

fn icon(data: &[u8]) -> Icon {
    Icon {
        width: ICON_SIZE,
        height: ICON_SIZE,
        data: data.to_vec(),
    }
}

/// DBusMenu treats underscores as access-key markers; double them to render.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

pub struct Handle {
    inner: ksni::blocking::Handle<OrcaTray>,
    last: Mutex<Model>,
}

impl Handle {
    /// Push a fresh snapshot; no-op (and no DBus traffic) when nothing the
    /// menu renders has changed.
    pub fn update(&self, store: &AgentStore, config: &Config) {
        let model = build_model(store, config, now_epoch());
        {
            let mut last = self.last.lock().unwrap();
            if *last == model {
                return;
            }
            *last = model.clone();
        }
        let _ = self.inner.update(move |tray| tray.model = model);
    }
}

pub fn spawn(tx: Sender<Msg>) -> Result<Handle, String> {
    let tray = OrcaTray {
        model: Model::default(),
        tx,
    };
    let inner = tray.spawn().map_err(|e| e.to_string())?;
    Ok(Handle {
        inner,
        last: Mutex::new(Model::default()),
    })
}

fn build_model(store: &AgentStore, config: &Config, now: f64) -> Model {
    Model {
        rows: store
            .agents()
            .iter()
            .map(|agent| AgentRow {
                id: agent.id.clone(),
                title: agent.title.clone(),
                source: agent.source.clone(),
                status: agent.status,
                duration: format_duration(agent.duration(now)),
                message: agent.message.clone(),
            })
            .collect(),
        running: store.running_count(),
        open: store.open_session_count(),
        prefs: config.notifications,
    }
}

/// Port of the macOS MenuView duration format: `57s`, `3m 12s`, `2h 5m`.
fn format_duration(interval: f64) -> String {
    let total = interval.max(0.0) as u64;
    if total < 60 {
        return format!("{total}s");
    }
    let minutes = total / 60;
    let seconds = total % 60;
    if minutes < 60 {
        return if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {seconds}s")
        };
    }
    let hours = minutes / 60;
    let mins = minutes % 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {mins}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_like_macos() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(57.0), "57s");
        assert_eq!(format_duration(60.0), "1m");
        assert_eq!(format_duration(192.0), "3m 12s");
        assert_eq!(format_duration(3600.0), "1h");
        assert_eq!(format_duration(7500.0), "2h 5m");
    }

    #[test]
    fn escapes_underscores_for_dbusmenu() {
        assert_eq!(escape("my_project"), "my__project");
    }
}

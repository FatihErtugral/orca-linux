//! Two StatusNotifierItems, mirroring the macOS status item pair: the dolphin
//! icon and, right beside it, the framed `running/open` counter. Plasma gives
//! every SNI a square cell, so the macOS wide item maps to two adjacent items;
//! the counter hides itself (Passive) while no session is open.

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

const NORMAL: (u8, u8, u8) = (0xE8, 0xE8, 0xE8);
const ATTENTION: (u8, u8, u8) = (0xFF, 0x9F, 0x0A);

/// Everything the tray renders, precomputed by the store loop. Snapshots are
/// compared before pushing so DBus only sees real changes.
#[derive(Clone, PartialEq, Default)]
struct Model {
    rows: Vec<AgentRow>,
    running: usize,
    open: usize,
    prefs: NotificationPreferences,
}

impl Model {
    fn attention(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.status, AgentStatus::Waiting | AgentStatus::Error))
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Orca".into(),
            description: format!("{} active · {} open", self.running, self.open),
            ..Default::default()
        }
    }
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

/// Both items answer clicks and carry the same menu; this seam lets the menu
/// builders stay generic over which one they live in.
trait TrayCtx: Sized {
    fn tx(&self) -> &Sender<Msg>;
}

pub struct OrcaTray {
    model: Model,
    icon: Vec<u8>,
    tx: Sender<Msg>,
}

impl TrayCtx for OrcaTray {
    fn tx(&self) -> &Sender<Msg> {
        &self.tx
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
        // The macOS design signals attention with a static color change.
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(self.icon.clone())]
    }

    fn tool_tip(&self) -> ToolTip {
        self.model.tool_tip()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        build_menu(&self.model)
    }
}

/// The `[running/open]` box — its own tray item, exactly like the macOS badge.
pub struct CounterTray {
    model: Model,
    icon: Vec<u8>,
    tx: Sender<Msg>,
}

impl TrayCtx for CounterTray {
    fn tx(&self) -> &Sender<Msg> {
        &self.tx
    }
}

impl ksni::Tray for CounterTray {
    fn id(&self) -> String {
        // Sorts next to "orca" in trays that order items by id.
        "orca-count".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Msg::TogglePopup);
    }

    fn title(&self) -> String {
        "Orca".into()
    }

    fn status(&self) -> Status {
        // Passive hides the item — no sessions, no counter, like macOS
        // hiding the badge when there is nothing to count.
        if self.model.open == 0 {
            Status::Passive
        } else {
            Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(self.icon.clone())]
    }

    fn tool_tip(&self) -> ToolTip {
        self.model.tool_tip()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        build_menu(&self.model)
    }
}

fn build_menu<T: TrayCtx + ksni::Tray>(model: &Model) -> Vec<MenuItem<T>> {
    let mut items: Vec<MenuItem<T>> = vec![
        MenuItem::Standard(StandardItem {
            label: format!("{} active · {} open", model.running, model.open),
            enabled: false,
            ..Default::default()
        }),
        MenuItem::Separator,
    ];

    if model.rows.is_empty() {
        items.push(MenuItem::Standard(StandardItem {
            label: "No active agents".into(),
            enabled: false,
            ..Default::default()
        }));
    }
    for row in &model.rows {
        items.push(agent_item(row));
    }

    items.push(MenuItem::Separator);
    items.push(settings_menu(&model.prefs));
    items.push(MenuItem::Separator);
    items.push(MenuItem::Standard(StandardItem {
        label: "Quit Orca".into(),
        activate: Box::new(|tray: &mut T| {
            let _ = tray.tx().send(Msg::Quit);
        }),
        ..Default::default()
    }));
    items
}

fn agent_item<T: TrayCtx + ksni::Tray>(row: &AgentRow) -> MenuItem<T> {
    let glyph = match row.status {
        AgentStatus::Running => "▶",
        AgentStatus::Waiting => "⏳",
        AgentStatus::Done => "✅",
        AgentStatus::Error => "❌",
        AgentStatus::Idle => "●",
    };
    let mut children: Vec<MenuItem<T>> = vec![MenuItem::Standard(StandardItem {
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
        activate: Box::new(move |tray: &mut T| {
            let _ = tray.tx().send(Msg::Dismiss(id.clone()));
        }),
        ..Default::default()
    }));
    let focus_id = row.id.clone();
    children.push(MenuItem::Standard(StandardItem {
        label: "Focus terminal".into(),
        activate: Box::new(move |tray: &mut T| {
            let _ = tray.tx().send(Msg::Focus(focus_id.clone()));
        }),
        ..Default::default()
    }));

    MenuItem::SubMenu(SubMenu {
        label: escape(&format!("{glyph} {} · {}", row.title, row.duration)),
        submenu: children,
        ..Default::default()
    })
}

fn settings_menu<T: TrayCtx + ksni::Tray>(prefs: &NotificationPreferences) -> MenuItem<T> {
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
                    activate: Box::new(move |tray: &mut T| {
                        let _ = tray.tx().send(Msg::SetPref(key, !checked));
                    }),
                    ..Default::default()
                })
            })
            .collect(),
        ..Default::default()
    })
}

fn icon(data: Vec<u8>) -> Icon {
    Icon {
        width: ICON_SIZE,
        height: ICON_SIZE,
        data,
    }
}

/// DBusMenu treats underscores as access-key markers; double them to render.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

pub struct Handle {
    dolphin: ksni::blocking::Handle<OrcaTray>,
    counter: ksni::blocking::Handle<CounterTray>,
    last: Mutex<Model>,
}

impl Handle {
    /// Push a fresh snapshot; no-op (and no DBus traffic) when nothing the
    /// tray renders has changed.
    pub fn update(&self, store: &AgentStore, config: &Config) {
        let model = build_model(store, config, now_epoch());
        {
            let mut last = self.last.lock().unwrap();
            if *last == model {
                return;
            }
            *last = model.clone();
        }
        let attention = model.attention();
        let dolphin_icon = if attention {
            ICON_ATTENTION.to_vec()
        } else {
            ICON_NORMAL.to_vec()
        };
        let color = if attention { ATTENTION } else { NORMAL };
        let counter_icon = crate::badge::counter_icon(model.running, model.open, color);

        let counter_model = model.clone();
        let _ = self.dolphin.update(move |tray| {
            tray.model = model;
            tray.icon = dolphin_icon;
        });
        let _ = self.counter.update(move |tray| {
            tray.model = counter_model;
            tray.icon = counter_icon;
        });
    }
}

pub fn spawn(tx: Sender<Msg>) -> Result<Handle, String> {
    let dolphin = OrcaTray {
        model: Model::default(),
        icon: ICON_NORMAL.to_vec(),
        tx: tx.clone(),
    };
    let counter = CounterTray {
        model: Model::default(),
        icon: crate::badge::counter_icon(0, 0, NORMAL),
        tx,
    };
    let dolphin = dolphin.spawn().map_err(|e| e.to_string())?;
    let counter = counter.spawn().map_err(|e| e.to_string())?;
    Ok(Handle {
        dolphin,
        counter,
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

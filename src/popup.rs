//! The popover window — the Linux counterpart of the macOS `NSPopover` +
//! `MenuView`. Left-clicking the tray icon toggles it; it hides on Esc or when
//! it loses focus. Rendering mirrors MenuView.swift: header with counts and a
//! gear, colored status dots, live-ticking durations, per-row dismiss, inline
//! settings pane with the five notification toggles and an update check.

use crate::daemon::prefs::NotificationPreferences;
use crate::daemon::{Msg, PrefKey};
use crate::protocol::AgentStatus;
use crate::version;
use eframe::egui;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct UiState {
    pub rows: Vec<UiAgent>,
    pub running: usize,
    pub open: usize,
    pub prefs: NotificationPreferences,
}

#[derive(Clone)]
pub struct UiAgent {
    pub id: String,
    pub title: String,
    pub source: String,
    pub status: AgentStatus,
    pub message: Option<String>,
    pub run_started_at: Option<f64>,
    pub last_run_duration: f64,
}

impl UiAgent {
    fn duration(&self, now: f64) -> f64 {
        if self.status == AgentStatus::Running {
            if let Some(start) = self.run_started_at {
                return (now - start).max(0.0);
            }
        }
        self.last_run_duration
    }
}

/// Bridge between the store loop (which owns the truth) and the UI thread.
pub struct SharedUi {
    pub state: Mutex<UiState>,
    pub ctx: OnceLock<egui::Context>,
    pub visible: AtomicBool,
}

impl Default for SharedUi {
    fn default() -> Self {
        Self {
            state: Mutex::new(UiState::default()),
            ctx: OnceLock::new(),
            visible: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, PartialEq)]
enum CheckState {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Installing(String),
    Failed,
}

pub fn run(shared: Arc<SharedUi>, tx: Sender<Msg>) -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([340.0, 420.0])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_visible(false)
            .with_transparent(true)
            .with_app_id("orca"),
        run_and_return: true,
        ..Default::default()
    };
    eframe::run_native(
        "Orca",
        options,
        Box::new(move |cc| {
            let _ = shared.ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(PopupApp {
                shared,
                tx,
                show_settings: false,
                was_focused: false,
                check: Arc::new(Mutex::new(CheckState::Idle)),
            }))
        }),
    )
}

struct PopupApp {
    shared: Arc<SharedUi>,
    tx: Sender<Msg>,
    show_settings: bool,
    was_focused: bool,
    check: Arc<Mutex<CheckState>>,
}

impl PopupApp {
    fn hide(&mut self, ctx: &egui::Context) {
        self.shared.visible.store(false, Ordering::Relaxed);
        self.show_settings = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

impl eframe::App for PopupApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent behind the rounded panel
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let visible = self.shared.visible.load(Ordering::Relaxed);
        if visible && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.hide(&ctx);
        }
        // Hide like a popover: on the transition from focused to unfocused.
        let focused = ctx.input(|i| i.focused);
        if visible && self.was_focused && !focused {
            self.hide(&ctx);
        }
        self.was_focused = focused;

        let state = self.shared.state.lock().unwrap().clone();
        let mut focus_clicked = false;
        let panel_frame = egui::Frame::window(ui.style())
            .corner_radius(12.0)
            .inner_margin(14.0);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                self.header(ui, &state);
                ui.separator();
                if self.show_settings {
                    self.settings(ui, &state.prefs);
                } else if state.rows.is_empty() {
                    ui.add_space(6.0);
                    ui.weak("No active agents");
                    ui.add_space(6.0);
                } else {
                    let now = crate::daemon::now_epoch();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for row in &state.rows {
                                if self.agent_row(ui, row, now) {
                                    focus_clicked = true;
                                }
                                ui.add_space(4.0);
                            }
                        });
                }
            });

        // Jumping to the terminal hands focus away, so close like a popover.
        if focus_clicked {
            self.hide(&ctx);
        }

        // Tick the durations only while someone can see them.
        if visible && state.rows.iter().any(|r| r.status == AgentStatus::Running) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}

impl PopupApp {
    fn header(&mut self, ui: &mut egui::Ui, state: &UiState) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Orca").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("⚙").frame(false))
                    .on_hover_text("Notification & sound settings")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
                ui.weak(format!("{} active · {} open", state.running, state.open));
            });
        });
    }

    /// Renders one agent row. Returns true when the row body was clicked
    /// (jump to the owning terminal, like the macOS popover).
    fn agent_row(&self, ui: &mut egui::Ui, row: &UiAgent, now: f64) -> bool {
        let response = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .id_salt(&row.id)
                    .sense(egui::Sense::click()),
                |ui| {
                    self.agent_row_contents(ui, row, now);
                },
            )
            .response;
        let clicked = response.clicked();
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if clicked {
            let _ = self.tx.send(Msg::Focus(row.id.clone()));
        }
        clicked
    }

    fn agent_row_contents(&self, ui: &mut egui::Ui, row: &UiAgent, now: f64) {
        ui.horizontal_top(|ui| {
            // Status dot, top-aligned like the macOS popover.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 18.0), egui::Sense::hover());
            ui.painter().circle_filled(
                egui::pos2(rect.center().x, rect.top() + 9.0),
                4.5,
                status_color(row.status),
            );

            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 22.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(truncated(&row.title, 34)).strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(
                            egui::RichText::new(format_duration(row.duration(now))).monospace(),
                        );
                    });
                });
                ui.weak(
                    egui::RichText::new(format!("{} · {}", row.source, row.status.label()))
                        .size(11.0),
                );
                if let Some(message) = &row.message {
                    if !message.is_empty() {
                        ui.weak(egui::RichText::new(truncated(message, 90)).size(10.0));
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("✕").size(11.0)).frame(false))
                    .on_hover_text("Dismiss")
                    .clicked()
                {
                    let _ = self.tx.send(Msg::Dismiss(row.id.clone()));
                }
            });
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui, prefs: &NotificationPreferences) {
        let mut prefs = *prefs;
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Notifications").strong());
        self.toggle(ui, "Enable notifications", prefs.enabled, PrefKey::Enabled, true, &mut prefs);
        ui.indent("per-status", |ui| {
            self.toggle(ui, "Waiting for input", prefs.on_waiting, PrefKey::OnWaiting, prefs.enabled, &mut prefs);
            self.toggle(ui, "Finished", prefs.on_done, PrefKey::OnDone, prefs.enabled, &mut prefs);
            self.toggle(ui, "Errors", prefs.on_error, PrefKey::OnError, prefs.enabled, &mut prefs);
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Sound").strong());
        self.toggle(ui, "Play sound", prefs.sound, PrefKey::Sound, prefs.enabled, &mut prefs);

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.weak(egui::RichText::new(format!("Orca v{}", version::CURRENT)).size(10.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let check = self.check.lock().unwrap().clone();
                let (label, enabled) = match &check {
                    CheckState::Idle => ("Check for updates".to_string(), true),
                    CheckState::Checking => ("Checking…".into(), false),
                    CheckState::UpToDate => ("Up to date ✓".into(), true),
                    CheckState::Available(v) => (format!("Install {v}"), true),
                    CheckState::Installing(v) => (format!("Installing {v}…"), false),
                    CheckState::Failed => ("Check failed — retry".into(), true),
                };
                let button = egui::Button::new(egui::RichText::new(label).size(10.0));
                if ui.add_enabled(enabled, button).clicked() {
                    self.on_update_button(&check, ui.ctx().clone());
                }
            });
        });
    }

    fn toggle(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        value: bool,
        key: PrefKey,
        enabled: bool,
        prefs: &mut NotificationPreferences,
    ) {
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = value;
                    if ui.checkbox(&mut on, "").changed() {
                        let _ = self.tx.send(Msg::SetPref(key, on));
                        // Optimistic local echo so the pane reacts instantly;
                        // the store loop pushes the canonical state right after.
                        apply_local(prefs, key, on);
                        if let Ok(mut state) = self.shared.state.lock() {
                            state.prefs = *prefs;
                        }
                    }
                });
            });
        });
    }

    fn on_update_button(&self, check: &CheckState, ctx: egui::Context) {
        let state = self.check.clone();
        match check {
            CheckState::Available(version) => {
                *state.lock().unwrap() = CheckState::Installing(version.clone());
                if let Ok(exe) = std::env::current_exe() {
                    let _ = Command::new(exe).arg("update").spawn();
                }
            }
            _ => {
                *state.lock().unwrap() = CheckState::Checking;
                std::thread::spawn(move || {
                    let result = std::env::current_exe().ok().and_then(|exe| {
                        Command::new(exe).args(["update", "--check"]).output().ok()
                    });
                    let next = match result {
                        Some(output) if output.status.success() => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            match stdout.split_whitespace().nth(1) {
                                Some(tag) if stdout.starts_with("update-available") => {
                                    CheckState::Available(tag.to_string())
                                }
                                _ => CheckState::UpToDate,
                            }
                        }
                        _ => CheckState::Failed,
                    };
                    *state.lock().unwrap() = next;
                    ctx.request_repaint();
                });
            }
        }
    }
}

fn apply_local(prefs: &mut NotificationPreferences, key: PrefKey, value: bool) {
    match key {
        PrefKey::Enabled => prefs.enabled = value,
        PrefKey::OnWaiting => prefs.on_waiting = value,
        PrefKey::OnDone => prefs.on_done = value,
        PrefKey::OnError => prefs.on_error = value,
        PrefKey::Sound => prefs.sound = value,
    }
}

fn status_color(status: AgentStatus) -> egui::Color32 {
    match status {
        AgentStatus::Running => egui::Color32::from_rgb(10, 132, 255),
        AgentStatus::Waiting => egui::Color32::from_rgb(255, 159, 10),
        AgentStatus::Done => egui::Color32::from_rgb(48, 209, 88),
        AgentStatus::Error => egui::Color32::from_rgb(255, 69, 58),
        AgentStatus::Idle => egui::Color32::GRAY,
    }
}

fn truncated(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    format!("{}…", cut.trim_end())
}

/// Port of the macOS MenuView duration format: `57s`, `3m 12s`, `2h 5m`.
pub fn format_duration(interval: f64) -> String {
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
    fn ui_agent_duration_ticks_only_while_running() {
        let mut agent = UiAgent {
            id: "a".into(),
            title: "t".into(),
            source: "s".into(),
            status: AgentStatus::Running,
            message: None,
            run_started_at: Some(100.0),
            last_run_duration: 7.0,
        };
        assert_eq!(agent.duration(105.0), 5.0);
        agent.status = AgentStatus::Waiting;
        assert_eq!(agent.duration(105.0), 7.0);
    }
}

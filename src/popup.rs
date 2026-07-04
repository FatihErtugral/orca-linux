//! The popover window — the Linux counterpart of the macOS `NSPopover` +
//! `MenuView`, running as its own short-lived process (`orca popup`).
//!
//! Wayland does not allow hiding/showing a window (winit's `set_visible` is a
//! no-op there), so visibility is modeled as process lifetime: the tray toggle
//! spawns this process and terminates it. State arrives as a JSON-line stream
//! over the daemon socket (`{"type":"subscribe"}`); user actions go back as
//! `{"type":"action",...}` lines. Closing (Esc, focus loss, jump-to-terminal)
//! simply exits.

use crate::daemon::prefs::NotificationPreferences;
use crate::daemon::PrefKey;
use crate::protocol::AgentStatus;
use crate::socket::{self, Action};
use crate::ui_state::{UiAgent, UiState};
use crate::{paths, version};
use eframe::egui;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub fn run_popup() -> i32 {
    let socket_path = paths::socket_path();
    let Ok(mut stream) = UnixStream::connect(&socket_path) else {
        eprintln!("orca: daemon is not running — start it with `orca tray`");
        return 1;
    };
    if stream.write_all(b"{\"type\":\"subscribe\"}\n").is_err() {
        eprintln!("orca: could not subscribe to the daemon");
        return 1;
    }

    let shared = Arc::new(Shared {
        state: Mutex::new(UiState::default()),
        ctx: OnceLock::new(),
    });

    // State stream: one UiState JSON line per change; EOF means the daemon
    // went away, which closes the popup.
    let reader_shared = shared.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(state) = serde_json::from_str::<UiState>(&line) {
                        *reader_shared.state.lock().unwrap() = state;
                        if let Some(ctx) = reader_shared.ctx.get() {
                            ctx.request_repaint();
                        }
                    }
                }
            }
        }
        if let Some(ctx) = reader_shared.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([340.0, 420.0])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_transparent(true)
            .with_app_id("orca"),
        run_and_return: false,
        ..Default::default()
    };
    let app_shared = shared.clone();
    let result = eframe::run_native(
        "Orca",
        options,
        Box::new(move |cc| {
            // Labels must not swallow clicks (text selection), otherwise the
            // row's click sense only fires in the gaps between texts.
            cc.egui_ctx
                .all_styles_mut(|style| style.interaction.selectable_labels = false);
            let _ = app_shared.ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(PopupApp {
                shared: app_shared,
                socket_path,
                show_settings: false,
                was_focused: false,
                closing: false,
                frames: 0,
                armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                check: Arc::new(Mutex::new(CheckState::Idle)),
            }))
        }),
    );
    if result.is_err() {
        eprintln!("orca: popup UI failed to start");
        return 1;
    }
    0
}

struct Shared {
    state: Mutex<UiState>,
    ctx: OnceLock<egui::Context>,
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

struct PopupApp {
    shared: Arc<Shared>,
    socket_path: PathBuf,
    show_settings: bool,
    was_focused: bool,
    closing: bool,
    /// Frames rendered so far — drives the event-based positioning pass.
    frames: u32,
    /// Set once positioning (which also settles focus via KWin) completed;
    /// only then may focus loss close the popup. Event-driven, no clocks.
    armed: Arc<std::sync::atomic::AtomicBool>,
    check: Arc<Mutex<CheckState>>,
}

impl PopupApp {
    fn send(&self, action: Action) {
        socket::send_action(&self.socket_path, &action);
    }

    fn close(&mut self, ctx: &egui::Context) {
        if self.closing {
            return;
        }
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for PopupApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent behind the rounded panel
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Dock at the tray corner, driven by frame events rather than timers:
        // frame 1 renders into the surface being mapped, frame 2 is
        // compositor-synced after the map, so KWin knows the window by then.
        // The script is idempotent; two passes cover both orderings. The
        // second pass also activates the window via KWin and arms the
        // focus-loss close below.
        if self.frames < 2 {
            self.frames += 1;
            let last_pass = self.frames == 2;
            let armed = self.armed.clone();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                crate::daemon::focus::position_popup_bottom_right();
                if last_pass {
                    armed.store(true, std::sync::atomic::Ordering::Relaxed);
                    repaint.request_repaint();
                }
            });
            ctx.request_repaint();
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close(&ctx);
        }
        // Close like a popover: on the transition from focused to unfocused —
        // but only after positioning settled the initial focus, so compositor
        // shuffle during mapping can never read as click-away.
        let focused = ctx.input(|i| i.focused);
        if focused != self.was_focused {
            log::debug!("focus transition: {} -> {focused}", self.was_focused);
        }
        if self.was_focused && !focused && self.armed.load(std::sync::atomic::Ordering::Relaxed) {
            self.close(&ctx);
        }
        self.was_focused = focused;

        let state = self.shared.state.lock().unwrap().clone();
        let mut focus_clicked = false;
        // No shadow: it would paint outside the rounded rect and show up as a
        // dark protrusion at the window edge (the surface itself is square).
        let panel_frame = egui::Frame::window(ui.style())
            .corner_radius(12.0)
            .inner_margin(14.0)
            .shadow(egui::Shadow::NONE);
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
            self.close(&ctx);
        }

        // Tick the durations while any agent is running.
        if state.rows.iter().any(|r| r.status == AgentStatus::Running) {
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
        // Reserve a background slot before the content so the hover highlight
        // renders behind the text instead of over it.
        let hover_bg = ui.painter().add(egui::Shape::Noop);
        let response = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .id_salt(&row.id)
                    .sense(egui::Sense::click()),
                |ui| {
                    // Claim the full row width so the whole line is clickable,
                    // not just the rect the text happens to occupy.
                    ui.set_min_width(ui.available_width());
                    self.agent_row_contents(ui, row, now);
                },
            )
            .response;
        let clicked = response.clicked();
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().set(
                hover_bg,
                egui::epaint::RectShape::filled(
                    response.rect.expand2(egui::vec2(6.0, 3.0)),
                    6.0,
                    ui.visuals().widgets.hovered.weak_bg_fill,
                ),
            );
        }
        if clicked {
            self.send(Action {
                action: "focus".into(),
                id: Some(row.id.clone()),
                key: None,
                value: None,
            });
        }
        clicked
    }

    fn agent_row_contents(&self, ui: &mut egui::Ui, row: &UiAgent, now: f64) {
        // Text-column height from the previous frame, so the dismiss button can
        // center against the row (egui is single-pass; one frame of lag is
        // invisible and converges immediately).
        let height_id = egui::Id::new(("row-height", &row.id));
        let text_height: f32 = ui.ctx().data(|d| d.get_temp(height_id)).unwrap_or(30.0);
        ui.horizontal_top(|ui| {
            // Status dot, top-aligned like the macOS popover.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 18.0), egui::Sense::hover());
            ui.painter().circle_filled(
                egui::pos2(rect.center().x, rect.top() + 9.0),
                4.5,
                status_color(row.status),
            );

            let text_column = ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 22.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(truncated(&row.title, 34)).strong());
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
            ui.ctx()
                .data_mut(|d| d.insert_temp(height_id, text_column.response.rect.height()));

            // Dismiss button, vertically centered against the text column.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                ui.allocate_ui(egui::vec2(16.0, text_height), |ui| {
                    ui.centered_and_justified(|ui| {
                        if close_button(ui).on_hover_text("Dismiss").clicked() {
                            self.send(Action {
                                action: "dismiss".into(),
                                id: Some(row.id.clone()),
                                key: None,
                                value: None,
                            });
                        }
                    });
                });
            });
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui, prefs: &NotificationPreferences) {
        let mut prefs = *prefs;
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Notifications").strong());
        self.toggle(
            ui,
            "Enable notifications",
            prefs.enabled,
            PrefKey::Enabled,
            true,
            &mut prefs,
        );
        ui.indent("per-status", |ui| {
            self.toggle(
                ui,
                "Waiting for input",
                prefs.on_waiting,
                PrefKey::OnWaiting,
                prefs.enabled,
                &mut prefs,
            );
            self.toggle(
                ui,
                "Finished",
                prefs.on_done,
                PrefKey::OnDone,
                prefs.enabled,
                &mut prefs,
            );
            self.toggle(
                ui,
                "Errors",
                prefs.on_error,
                PrefKey::OnError,
                prefs.enabled,
                &mut prefs,
            );
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Sound").strong());
        self.toggle(
            ui,
            "Play sound",
            prefs.sound,
            PrefKey::Sound,
            prefs.enabled,
            &mut prefs,
        );

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
                    if toggle_switch(ui, &mut on).changed() {
                        self.send(Action {
                            action: "set_pref".into(),
                            id: None,
                            key: Some(key.as_str().into()),
                            value: Some(on),
                        });
                        // Optimistic local echo so the pane reacts instantly;
                        // the daemon streams the canonical state right after.
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

/// iOS/Plasma-style animated toggle switch (the canonical egui custom widget);
/// the default font has no switch, and checkboxes read as foreign here.
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = egui::vec2(30.0, 16.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        ui.painter().circle(
            egui::pos2(circle_x, rect.center().y),
            0.75 * radius,
            visuals.fg_stroke.color,
            visuals.fg_stroke,
        );
    }
    response
}

/// A small "✕", drawn with the painter — the bundled fonts lack the glyph
/// (it rendered as a placeholder box).
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let r = 3.5;
        let c = rect.center();
        let stroke = egui::Stroke::new(1.4, visuals.fg_stroke.color);
        ui.painter()
            .line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke);
        ui.painter()
            .line_segment([c + egui::vec2(r, -r), c + egui::vec2(-r, r)], stroke);
    }
    response
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
    fn escapes_are_not_needed_but_truncation_is_stable() {
        assert_eq!(truncated("short", 34), "short");
        let long = "a".repeat(50);
        assert!(truncated(&long, 34).ends_with('…'));
    }
}

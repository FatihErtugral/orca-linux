pub mod notify;
pub mod prefs;
pub mod store;
pub mod title_refresher;

use crate::popup::{SharedUi, UiAgent, UiState};
use crate::protocol::AgentEvent;
use crate::state_store::StateStore;
use crate::{args, paths, socket};
use eframe::egui;
use notify::{DesktopNotifier, Notifier};
use prefs::Config;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use store::AgentStore;
use title_refresher::TitleRefresher;

/// Everything that can reach the store loop — the single place state mutates.
#[allow(clippy::large_enum_variant)] // Msg volume is tiny; boxing buys nothing
pub enum Msg {
    Event(AgentEvent),
    Dismiss(String),
    SetPref(PrefKey, bool),
    TogglePopup,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefKey {
    Enabled,
    OnWaiting,
    OnDone,
    OnError,
    Sound,
}

const TICK: Duration = Duration::from_secs(1);
const MAINTENANCE_EVERY: u64 = 3; // seconds, mirroring the macOS 3s timer

pub fn run(argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    let no_tray = parsed.flags.contains_key("no-tray");

    let socket_path = paths::socket_path();
    let server = match socket::Server::bind(&socket_path) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("orca: {error}");
            return 1;
        }
    };
    log::info!("listening on {}", socket_path.display());

    let (tx, rx) = mpsc::channel::<Msg>();

    // Socket thread: decoded events are forwarded into the store loop.
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>();
    thread::spawn(move || server.run(event_tx));
    let forward = tx.clone();
    thread::spawn(move || {
        for event in event_rx {
            if forward.send(Msg::Event(event)).is_err() {
                break;
            }
        }
    });

    let state_store = StateStore::default_store();
    let mut store = AgentStore::default();
    let config = Config::load(&paths::config_path());

    // Rehydrate sessions that were already open before the daemon launched.
    for event in state_store.load_all(1800.0, now_epoch(), &AgentStore::is_process_alive) {
        store.apply(&event, now_epoch());
    }
    log::info!("rehydrated {} session(s)", store.agents().len());

    if no_tray {
        store_loop(rx, store, state_store, config, None, None);
        let _ = std::fs::remove_file(&socket_path);
        return 0;
    }

    let tray = match crate::tray::spawn(tx.clone()) {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!("orca: could not start tray: {error} (popover still available)");
            None
        }
    };

    let shared = Arc::new(SharedUi::default());
    let worker_shared = shared.clone();
    let worker = thread::spawn(move || {
        store_loop(rx, store, state_store, config, tray, Some(worker_shared));
    });

    // The popover runs on the main thread (winit requirement). If it cannot
    // start (e.g. no GL), fall back to tray-menu-only operation.
    match crate::popup::run(shared, tx.clone()) {
        Ok(()) => {
            let _ = tx.send(Msg::Quit);
        }
        Err(error) => {
            eprintln!("orca: popover UI unavailable: {error} (tray menu still works)");
        }
    }
    let _ = worker.join();
    let _ = std::fs::remove_file(&socket_path);
    0
}

fn store_loop(
    rx: mpsc::Receiver<Msg>,
    mut store: AgentStore,
    state_store: StateStore,
    mut config: Config,
    tray: Option<crate::tray::Handle>,
    shared: Option<Arc<SharedUi>>,
) {
    let notifier = DesktopNotifier;
    let mut refresher = TitleRefresher::system();
    push_snapshots(&tray, &shared, &store, &config);

    let mut next_tick = Instant::now() + TICK;
    let mut tick_count: u64 = 0;
    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(Msg::Event(event)) => {
                log::debug!("event: {} {} ({})", event.id, event.status, event.source);
                if let Some(notification) = store.apply(&event, now_epoch()) {
                    dispatch(&notifier, &config, &notification);
                }
                push_snapshots(&tray, &shared, &store, &config);
            }
            Ok(Msg::Dismiss(id)) => {
                store.remove(&id);
                state_store.remove(&id);
                push_snapshots(&tray, &shared, &store, &config);
            }
            Ok(Msg::SetPref(key, value)) => {
                apply_pref(&mut config, key, value);
                config.save(&paths::config_path());
                push_snapshots(&tray, &shared, &store, &config);
            }
            Ok(Msg::TogglePopup) => toggle_popup(&shared),
            Ok(Msg::Quit) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                next_tick = Instant::now() + TICK;
                tick_count += 1;
                if tick_count.is_multiple_of(MAINTENANCE_EVERY) {
                    let mut changed = store.evaluate_health(&AgentStore::is_process_alive);
                    store.prune(now_epoch());
                    changed |= refresher.refresh(&mut store);
                    let _ = changed;
                }
                push_snapshots(&tray, &shared, &store, &config);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Close the popover so eframe's run loop returns and the process exits.
    if let Some(shared) = &shared {
        if let Some(ctx) = shared.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
    }
}

fn push_snapshots(
    tray: &Option<crate::tray::Handle>,
    shared: &Option<Arc<SharedUi>>,
    store: &AgentStore,
    config: &Config,
) {
    if let Some(handle) = tray {
        handle.update(store, config);
    }
    if let Some(shared) = shared {
        let state = UiState {
            rows: store
                .agents()
                .iter()
                .map(|agent| UiAgent {
                    id: agent.id.clone(),
                    title: agent.title.clone(),
                    source: agent.source.clone(),
                    status: agent.status,
                    message: agent.message.clone(),
                    run_started_at: agent.run_started_at,
                    last_run_duration: agent.last_run_duration,
                })
                .collect(),
            running: store.running_count(),
            open: store.open_session_count(),
            prefs: config.notifications,
        };
        let changed = {
            let mut current = shared.state.lock().unwrap();
            let changed = !same_state(&current, &state);
            *current = state;
            changed
        };
        if changed && shared.visible.load(Ordering::Relaxed) {
            if let Some(ctx) = shared.ctx.get() {
                ctx.request_repaint();
            }
        }
    }
}

/// Durations tick locally in the UI, so two states are "the same" when
/// everything except the wall clock matches.
fn same_state(a: &UiState, b: &UiState) -> bool {
    a.running == b.running
        && a.open == b.open
        && a.prefs == b.prefs
        && a.rows.len() == b.rows.len()
        && a.rows.iter().zip(&b.rows).all(|(x, y)| {
            x.id == y.id
                && x.title == y.title
                && x.status == y.status
                && x.message == y.message
                && x.run_started_at == y.run_started_at
        })
}

fn toggle_popup(shared: &Option<Arc<SharedUi>>) {
    let Some(shared) = shared else { return };
    let Some(ctx) = shared.ctx.get() else { return };
    let show = !shared.visible.load(Ordering::Relaxed);
    shared.visible.store(show, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(show));
    if show {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    ctx.request_repaint();
}

fn dispatch(notifier: &dyn Notifier, config: &Config, notification: &store::Notification) {
    if !config.notifications.should_notify(notification.status) {
        return;
    }
    if let Some((title, body)) = notify::format(notification) {
        notifier.notify(&title, &body, config.notifications.sound);
    }
}

fn apply_pref(config: &mut Config, key: PrefKey, value: bool) {
    let prefs = &mut config.notifications;
    match key {
        PrefKey::Enabled => prefs.enabled = value,
        PrefKey::OnWaiting => prefs.on_waiting = value,
        PrefKey::OnDone => prefs.on_done = value,
        PrefKey::OnError => prefs.on_error = value,
        PrefKey::Sound => prefs.sound = value,
    }
}

pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

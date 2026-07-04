pub mod notify;
pub mod prefs;
pub mod store;
pub mod title_refresher;

use crate::protocol::AgentEvent;
use crate::state_store::StateStore;
use crate::{args, paths, socket};
use notify::{DesktopNotifier, Notifier};
use prefs::Config;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use store::AgentStore;
use title_refresher::TitleRefresher;

/// Everything that can reach the store loop — the single place state mutates.
pub enum Msg {
    Event(AgentEvent),
    Dismiss(String),
    SetPref(PrefKey, bool),
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
    let mut config = Config::load(&paths::config_path());
    let notifier = DesktopNotifier;
    let mut refresher = TitleRefresher::system();

    // Rehydrate sessions that were already open before the daemon launched.
    for event in state_store.load_all(1800.0, now_epoch(), &AgentStore::is_process_alive) {
        store.apply(&event, now_epoch());
    }
    log::info!("rehydrated {} session(s)", store.agents().len());

    let tray = if no_tray {
        None
    } else {
        match crate::tray::spawn(tx.clone()) {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("orca: could not start tray: {error} (running headless)");
                None
            }
        }
    };
    push_tray_snapshot(&tray, &store, &config);

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
                push_tray_snapshot(&tray, &store, &config);
            }
            Ok(Msg::Dismiss(id)) => {
                store.remove(&id);
                state_store.remove(&id);
                push_tray_snapshot(&tray, &store, &config);
            }
            Ok(Msg::SetPref(key, value)) => {
                apply_pref(&mut config, key, value);
                config.save(&paths::config_path());
                push_tray_snapshot(&tray, &store, &config);
            }
            Ok(Msg::Quit) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                next_tick = Instant::now() + TICK;
                tick_count += 1;
                if tick_count % MAINTENANCE_EVERY == 0 {
                    let mut changed = store.evaluate_health(&AgentStore::is_process_alive);
                    store.prune(now_epoch());
                    changed |= refresher.refresh(&mut store);
                    if changed {
                        log::debug!("maintenance changed store state");
                    }
                }
                push_tray_snapshot(&tray, &store, &config);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    0
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

fn push_tray_snapshot(tray: &Option<crate::tray::Handle>, store: &AgentStore, config: &Config) {
    if let Some(handle) = tray {
        handle.update(store, config);
    }
}

pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

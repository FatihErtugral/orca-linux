pub mod focus;
pub mod notify;
pub mod prefs;
pub mod store;
pub mod title_refresher;

use crate::protocol::AgentEvent;
use crate::socket::{Action, Inbound, StateSink};
use crate::state_store::StateStore;
use crate::ui_state::{UiAgent, UiState};
use crate::{args, paths, socket};
use notify::{DesktopNotifier, Notifier};
use prefs::Config;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use store::AgentStore;
use title_refresher::TitleRefresher;

/// Everything that can reach the store loop — the single place state mutates.
#[allow(clippy::large_enum_variant)] // Msg volume is tiny; boxing buys nothing
pub enum Msg {
    Event(AgentEvent),
    Subscribe(Box<dyn StateSink>),
    Dismiss(String),
    Focus(String),
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

impl PrefKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::OnWaiting => "on_waiting",
            Self::OnDone => "on_done",
            Self::OnError => "on_error",
            Self::Sound => "sound",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "enabled" => Some(Self::Enabled),
            "on_waiting" => Some(Self::OnWaiting),
            "on_done" => Some(Self::OnDone),
            "on_error" => Some(Self::OnError),
            "sound" => Some(Self::Sound),
            _ => None,
        }
    }
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

    // Socket thread: decoded messages are forwarded into the store loop.
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>();
    // Loopback WebSocket for the Plasma plasmoid (QML has no unix sockets).
    match crate::ws::spawn(inbound_tx.clone()) {
        Some(port) => log::info!("plasmoid websocket on 127.0.0.1:{port}"),
        None => log::warn!("no free plasmoid websocket port in {:?}", crate::ws::PORTS),
    }
    thread::spawn(move || server.run(inbound_tx));
    let forward = tx.clone();
    thread::spawn(move || {
        for inbound in inbound_rx {
            let msg = match inbound {
                Inbound::Event(event) => Msg::Event(event),
                Inbound::Subscribe(sink) => Msg::Subscribe(sink),
                Inbound::Action(action) => match map_action(action) {
                    Some(msg) => msg,
                    None => continue,
                },
            };
            if forward.send(msg).is_err() {
                break;
            }
        }
    });

    let state_store = StateStore::default_store();
    let mut store = AgentStore::default();
    let mut config = Config::load(&paths::config_path());

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

    let notifier = DesktopNotifier;
    let mut refresher = TitleRefresher::system();
    let mut subscribers: Vec<Box<dyn StateSink>> = Vec::new();
    let mut last_ui = UiState::default();
    let mut popup = PopupProcess::default();

    push_snapshots(&tray, &mut subscribers, &mut last_ui, &store, &config, true);

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
                push_snapshots(
                    &tray,
                    &mut subscribers,
                    &mut last_ui,
                    &store,
                    &config,
                    false,
                );
            }
            Ok(Msg::Subscribe(sink)) => {
                subscribe(sink, &last_ui, &mut subscribers);
            }
            Ok(Msg::Dismiss(id)) => {
                store.remove(&id);
                state_store.remove(&id);
                push_snapshots(
                    &tray,
                    &mut subscribers,
                    &mut last_ui,
                    &store,
                    &config,
                    false,
                );
            }
            Ok(Msg::Focus(id)) => {
                if let Some(agent) = store.get(&id) {
                    let target = focus::FocusTarget {
                        pid: agent.pid,
                        cwd: agent.cwd.clone(),
                        term_program: agent.term_program.clone(),
                    };
                    // Shell-outs to busctl must not stall the store loop.
                    thread::spawn(move || focus::focus(&target));
                }
            }
            Ok(Msg::SetPref(key, value)) => {
                apply_pref(&mut config, key, value);
                config.save(&paths::config_path());
                push_snapshots(
                    &tray,
                    &mut subscribers,
                    &mut last_ui,
                    &store,
                    &config,
                    false,
                );
            }
            Ok(Msg::TogglePopup) => popup.toggle(),
            Ok(Msg::Quit) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                next_tick = Instant::now() + TICK;
                tick_count += 1;
                popup.reap();
                if tick_count.is_multiple_of(MAINTENANCE_EVERY) {
                    store.evaluate_health(&AgentStore::is_process_alive);
                    store.prune(now_epoch());
                    refresher.refresh(&mut store);
                }
                push_snapshots(
                    &tray,
                    &mut subscribers,
                    &mut last_ui,
                    &store,
                    &config,
                    false,
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    popup.terminate();
    let _ = std::fs::remove_file(&socket_path);
    0
}

fn map_action(action: Action) -> Option<Msg> {
    match action.action.as_str() {
        "dismiss" => action.id.map(Msg::Dismiss),
        "focus" => action.id.map(Msg::Focus),
        "set_pref" => {
            let key = PrefKey::parse(action.key.as_deref()?)?;
            Some(Msg::SetPref(key, action.value?))
        }
        "quit" => Some(Msg::Quit),
        _ => None,
    }
}

/// The popover lives in its own short-lived process: window lifetime equals
/// visibility, which sidesteps Wayland's unsupported hide/show entirely and
/// keeps the daemon free of any GL/UI footprint. Toggling is deterministic
/// process ownership — child alive means close it, otherwise open one; no
/// clocks, no grace windows.
#[derive(Default)]
struct PopupProcess {
    child: Option<Child>,
}

impl PopupProcess {
    fn toggle(&mut self) {
        self.reap();
        if self.child.is_some() {
            self.terminate();
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        match Command::new(exe)
            .arg("popup")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => self.child = Some(child),
            Err(error) => log::warn!("could not launch popup: {error}"),
        }
    }

    fn reap(&mut self) {
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
                self.child = None;
            }
        }
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            if matches!(child.try_wait(), Ok(None)) {
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
            }
        }
    }
}

fn subscribe(
    mut sink: Box<dyn StateSink>,
    last_ui: &UiState,
    subscribers: &mut Vec<Box<dyn StateSink>>,
) {
    if let Ok(json) = serde_json::to_string(last_ui) {
        if sink.send_state(&json) {
            subscribers.push(sink);
        }
    }
}

fn push_snapshots(
    tray: &Option<crate::tray::Handle>,
    subscribers: &mut Vec<Box<dyn StateSink>>,
    last_ui: &mut UiState,
    store: &AgentStore,
    config: &Config,
    force: bool,
) {
    if let Some(handle) = tray {
        handle.update(store, config);
    }
    let state = build_ui_state(store, config);
    if !force && state == *last_ui {
        return;
    }
    if let Ok(json) = serde_json::to_string(&state) {
        subscribers.retain_mut(|sink| sink.send_state(&json));
    }
    *last_ui = state;
}

fn build_ui_state(store: &AgentStore, config: &Config) -> UiState {
    UiState {
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
    }
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

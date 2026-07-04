# Orca for Linux — architecture & project rules

**KDE Plasma 6 port** of Orca (macOS, `github.com/FatihErtugral/orca`): a
panel status tracker for CLI AI agents. KDE-only by decision — the panel face
is a native plasmoid; do not add GNOME/other-DE support paths. One binary:
`tray` (daemon), `popup` (fallback popover process), `event`/`wrap` (CLI
producers), plus `install-hooks`/`uninstall-hooks`/`update`.

## Architecture (read this before touching anything)

```
hooks / wrap ──orca event──► UDS $XDG_RUNTIME_DIR/orca.sock
                                  │ NDJSON, one message per line
                                  ▼
                     daemon (orca tray) ──ws.rs──► loopback WebSocket 41957-66
                     single store loop thread            │ subscribe/action
                     (only mutator of AgentStore)        ▼
                                  ▲              Plasma plasmoid (plasmoid/)
                                  │               dolphin + [1/3] + native popup
                                  │
                     fallback UI (no --no-tray): ksni SNI pair + `orca popup`
                     process (eframe; process lifetime == visibility)
```

The plasmoid is the primary UI (daemon runs with `--no-tray`); the SNI pair +
popup process remain as the fallback when the plasmoid is not installed. Both
consume the same subscribe/action protocol (`StateSink` seam in socket.rs).

- **`daemon/store.rs`** — pure state machine (port of macOS `AgentStore`):
  no I/O, no clock; `now` is a parameter, liveness is an injected predicate,
  notifications come back as values (`Notification`). All behavior rules
  (prune tiers, run-timing freeze, sort order) live here and are unit-tested.
- **Store loop** (`daemon/mod.rs`) is the ONLY place state mutates. Everything
  reaches it as a `Msg` over one mpsc channel: socket events, tray clicks,
  popup actions. Never mutate store state from another thread.
- **Wire protocol** (`protocol.rs`): field names are frozen — they match the
  macOS `AgentEvent.swift` byte-for-byte. Control messages (`{"type":
  "subscribe"|"action"}`) are additive; plain event lines must keep decoding.
- **Popup is a separate process** on purpose: winit's `set_visible` is a no-op
  on Wayland, so visibility is modeled as process lifetime. Do not move the
  popover back into the daemon.
- **Wayland rules**: clients cannot position/raise/hide windows. Anything of
  that sort goes through KWin: scripting DBus (`daemon/focus.rs`) or the
  installed window rule (see `install.sh`). Never assume X11 tools (xdotool).
- **`unsafe` policy**: only thin libc FFI (`kill`, `poll`, `isatty`, `getppid`,
  `ttyname`, `getuid`), each a single expression wrapped in a safe function.
  New `unsafe` needs a comment justifying it.

## macOS parity

This is a port, not a fork: behavior must match the macOS implementation
(`/…/orca` repo is the spec — `AgentStore.swift`, `EventBuilder.swift`,
`HookInstaller.swift`, `ClaudeTranscript.swift`). Numbers that matter:
doneGrace 90s, staleTTL 1800s, maintenance every 3s, hook stdin poll 300ms,
title truncation 60 chars, duration format `57s`/`3m 12s`/`2h 5m`. If you
change behavior, check the Swift source first.

## Commands

```sh
make build     # debug build
make test      # cargo test (83+ unit/integration tests)
make lint      # cargo clippy --all-targets -- -D warnings  (CI-enforced)
make fmt       # rustfmt
make install   # release build → ~/.local/bin/orca
RUST_LOG=debug cargo run -- tray --no-tray   # headless daemon for debugging
scripts/release.sh vX.Y.Z   # container build (glibc 2.36) + gh release
```

Dev daemon next to production: `ORCA_SOCKET`, `ORCA_STATE_DIR`, `ORCA_CONFIG`.

## Conventions

- Zero clippy warnings (`-D warnings`); rustfmt before committing; CI runs
  fmt-check + clippy + tests on every push.
- Every behavioral rule gets a unit test; tests ported from the Swift suites
  keep their intent (see test names in `daemon/store.rs`).
- Dependency budget is deliberate: no tokio (std threads + mpsc suffice), no
  clap (30-line parser matches macOS flag grammar), image decoding only in
  `build.rs`. Justify any new dependency in the PR/commit message.
- Time is `f64` epoch seconds everywhere (matches the wire `ts`); durations
  tick locally in UIs from `run_started_at`, the daemon never pushes
  clock-only changes.
- `pkill -f "orca …"` in scripts/tests kills your own shell (the pattern
  matches the command line); use `pkill -x orca` or the `[o]rca` bracket trick.

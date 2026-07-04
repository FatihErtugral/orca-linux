# Orca for Linux (KDE Plasma)

Panel status tracker for CLI AI agents — the **KDE Plasma** port of
[Orca for macOS](https://github.com/FatihErtugral/orca). One glance at the
panel tells you which agent sessions are running, which are waiting for your
input, and for how long. Desktop notifications fire the moment an agent needs
you.

Orca does **not** read prompts, responses or any model traffic. It only tracks
session *state* (running / waiting / done / error) reported by lifecycle hooks.

> Orca for Linux targets **KDE Plasma 6 only** (Wayland or X11). The panel
> widget is a native plasmoid; there is no GNOME support.

## How it works

```
Claude Code hooks ──┐
orca wrap <cmd>  ───┼──> orca event ──> unix socket ──────> orca tray (daemon)
                    │      (CLI)      $XDG_RUNTIME_DIR        │            │
                    └── JSON, one line per event ─────────────┘            │
                                                                loopback websocket
                                                                           │
                                              Plasma panel widget (plasmoid)
                                              dolphin + [running/open] + popup
```

- **Plasmoid** — the panel face: dolphin icon plus the framed `running/open`
  counter as one wide element (like the clock); clicking opens a native,
  panel-anchored popup with the session list and settings. It subscribes to
  the daemon over a loopback WebSocket and pushes actions back the same way.
- **`orca tray`** — the daemon: socket server, state machine, notifications
  (`org.freedesktop.Notifications` + sound). Run it with `--no-tray` when the
  plasmoid is your UI; without the flag it also provides a classic
  StatusNotifierItem pair + standalone popover as a fallback interface.
- **`orca event`** — fire-and-forget client used by hooks; if the daemon is not
  running it silently no-ops so your agent never breaks. Also persists
  open-session state so a later daemon launch rediscovers live sessions.
- **`orca wrap -- <cmd>`** — track any CLI command: emits `running`, waits,
  then `done`/`error` from the exit code.

## Install

Quick install (downloads the latest release, installs the plasmoid, Claude
Code hooks and the systemd user service; falls back to a source build when no
compatible binary exists):

```sh
curl -fsSL https://raw.githubusercontent.com/FatihErtugral/orca-linux/master/install.sh | bash
```

Then add the **Orca** widget to your panel (right-click panel → Add Widgets →
Orca) — the installer also tries to add it to the bottom panel automatically.

From source (needs `rustup`):

```sh
make install            # cargo build --release + install to ~/.local/bin/orca
make install-plasmoid   # kpackagetool6 install/upgrade of the panel widget
orca install-hooks      # add Claude Code hooks to ~/.claude/settings.json
install -Dm644 packaging/orca.service ~/.config/systemd/user/orca.service
systemctl --user enable --now orca
```

Update later with `orca update` (checks GitHub releases, swaps the binary,
refreshes hooks, restarts the daemon).

## Requirements

- **KDE Plasma 6** (Wayland or X11) with `kpackagetool6` (part of Plasma).
- QtWebSockets QML module (`qt6-websockets`; preinstalled on most distros).
- Prebuilt release binaries target glibc 2.36+ (Debian 12+, Ubuntu 22.10+,
  Fedora 37+, Arch/openSUSE TW). Older systems: `install.sh` builds from
  source automatically (needs `rustup`).

## Connecting sources

**Claude Code** — `orca install-hooks` merges these into `~/.claude/settings.json`
(idempotent; `orca uninstall-hooks` removes only Orca's entries):

| Hook | Status |
|---|---|
| SessionStart, UserPromptSubmit | running |
| Notification | waiting — "Waiting for your input" |
| Stop | waiting — "Response ready, your turn" |
| SessionEnd | closed (removes the session) |

A `Stop` that fires while background tasks are pending stays `running`
("Working in background"). Session titles come from the transcript, matching
the `/resume` picker (rename > AI title > summary > first user message).

**Any CLI agent** — `orca wrap --title build -- cargo build` or add a shell
helper: `ab() { orca wrap -- "$@"; }`.

**Manual** — `orca event --id x --status waiting --message "look at me"`.

## Wire protocol

One JSON object per line over `$XDG_RUNTIME_DIR/orca.sock`, identical field
names to the macOS implementation:

```json
{"id":"<session>","source":"claude-code","status":"running","title":"…",
 "cwd":"…","message":"…","ts":1719400000.1,"tty":"/dev/pts/3",
 "term_program":"konsole","pid":12345,"transcript_path":"…"}
```

Statuses: `running`, `waiting`, `done`, `error`, `idle`; `closed`/`ended`
remove the session. Unknown statuses count as `running`.

## Behavior notes

- Sessions carrying a pid live exactly as long as that process; the daemon
  checks liveness every 3s. Pid-less sessions age out after 30 min.
- `done`/`idle` rows disappear 90s after their last update.
- Preferences (5 notification toggles) live in `~/.config/orca/config.toml`,
  editable from the tray's Settings submenu.
- Open-session state is persisted under `~/.local/state/orca/agents/` so a
  daemon restart rehydrates live sessions.
- Paths honor `ORCA_SOCKET`, `ORCA_STATE_DIR`, `ORCA_CONFIG` overrides — handy
  for running a dev daemon next to the real one.

## Development

```sh
make build   # debug build
make test    # unit + integration tests
make lint    # clippy -D warnings
RUST_LOG=debug cargo run -- tray --no-tray   # headless daemon for debugging
```

Releases: `scripts/release.sh vX.Y.Z` builds the asset in an older-glibc
container (via docker) and publishes the GitHub release.

Roadmap (post-MVP): click-to-focus of the owning terminal (KWin/Konsole DBus,
`vscode://` handlers), ollama polling, count badge rendered into the tray icon,
AUR package.

## License

PolyForm Noncommercial 1.0.0 — same as the macOS original.

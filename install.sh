#!/usr/bin/env bash
# Orca for Linux — quick install:
#   curl -fsSL https://raw.githubusercontent.com/FatihErtugral/orca-linux/master/install.sh | bash
set -euo pipefail

REPO="FatihErtugral/orca-linux"
BIN_DIR="${ORCA_BIN_DIR:-$HOME/.local/bin}"

if [ "$(uname -s)" != "Linux" ]; then
    echo "orca: this installer is Linux-only (for macOS see github.com/FatihErtugral/orca)" >&2
    exit 1
fi

ARCH="$(uname -m)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

build_from_source() {
    echo "==> Building from source"
    if ! command -v cargo >/dev/null 2>&1; then
        echo "orca: cargo not found — install Rust first: https://rustup.rs" >&2
        echo "  (or on Arch: sudo pacman -S rustup && rustup default stable)" >&2
        exit 1
    fi
    git clone --depth 1 "https://github.com/$REPO" "$TMP/src"
    (cd "$TMP/src" && cargo build --release)
    install -Dm755 "$TMP/src/target/release/orca" "$BIN_DIR/orca"
}

install_prebuilt() {
    local tag asset
    tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | grep -o '"tag_name": *"[^"]*"' | head -1 | cut -d'"' -f4)" || return 1
    [ -n "$tag" ] || return 1
    asset="orca-linux-$ARCH.tar.gz"
    echo "==> Downloading $asset ($tag)"
    curl -fsSL -o "$TMP/$asset" \
        "https://github.com/$REPO/releases/download/$tag/$asset" || return 1
    tar -xzf "$TMP/$asset" -C "$TMP"
    # Older glibc than the build host? Fall back to a source build.
    "$TMP/orca" --version >/dev/null 2>&1 || return 1
    install -Dm755 "$TMP/orca" "$BIN_DIR/orca"
}

if ! install_prebuilt; then
    echo "==> Prebuilt binary unavailable or incompatible with this system"
    build_from_source
fi
echo "==> Installed $("$BIN_DIR/orca" --version) -> $BIN_DIR/orca"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: add $BIN_DIR to your PATH" ;;
esac

echo "==> Installing Claude Code hooks"
"$BIN_DIR/orca" install-hooks

# KDE: a window rule keeps the popover out of the taskbar/switcher and above
# other windows — declarative, no runtime scripting needed for that part.
if command -v kwriteconfig6 >/dev/null 2>&1 && ! grep -q "Orca popup" "$HOME/.config/kwinrulesrc" 2>/dev/null; then
    echo "==> Adding KWin window rule for the popover"
    UUID="$(cat /proc/sys/kernel/random/uuid)"
    EXISTING="$(kreadconfig6 --file kwinrulesrc --group General --key rules 2>/dev/null || true)"
    COUNT="$(kreadconfig6 --file kwinrulesrc --group General --key count 2>/dev/null || echo 0)"
    for pair in "Description:Orca popup" "wmclass:orca" "wmclassmatch:1" \
                "skiptaskbar:true" "skiptaskbarrule:2" "skipswitcher:true" "skipswitcherrule:2" \
                "skippager:true" "skippagerrule:2" "above:true" "aboverule:2"; do
        kwriteconfig6 --file kwinrulesrc --group "$UUID" --key "${pair%%:*}" "${pair#*:}"
    done
    kwriteconfig6 --file kwinrulesrc --group General --key count $(( ${COUNT:-0} + 1 ))
    kwriteconfig6 --file kwinrulesrc --group General --key rules "${EXISTING:+$EXISTING,}$UUID"
    busctl --user call org.kde.KWin /KWin org.kde.KWin reconfigure >/dev/null 2>&1 || true
fi

if command -v systemctl >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
    echo "==> Setting up systemd user service"
    install -Dm644 /dev/stdin "$HOME/.config/systemd/user/orca.service" <<UNIT
[Unit]
Description=Orca agent tray
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=$BIN_DIR/orca tray
Restart=on-failure

[Install]
WantedBy=graphical-session.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable --now orca.service
    echo "==> orca.service enabled (systemctl --user status orca)"
else
    echo "==> Start the tray with: orca tray &"
fi

echo "Done. Restart Claude Code (or open /hooks) so the hooks take effect."

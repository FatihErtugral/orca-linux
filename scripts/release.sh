#!/usr/bin/env bash
# Build the release tarball and publish a GitHub release.
#   scripts/release.sh v0.1.0
# The binary is built inside an older-glibc container when docker is
# available, so the asset runs on non-rolling distros too.
set -euo pipefail

TAG="${1:?usage: scripts/release.sh vX.Y.Z}"
VERSION="$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)"
if [ "$TAG" != "v$VERSION" ]; then
    echo "release: tag $TAG does not match Cargo.toml version v$VERSION" >&2
    exit 1
fi

ARCH="$(uname -m)"
DIST="dist"
rm -rf "$DIST" && mkdir -p "$DIST"

cargo test
cargo clippy --all-targets -- -D warnings

if command -v docker >/dev/null 2>&1; then
    echo "==> Building in rust:bookworm (glibc 2.36 baseline)"
    docker run --rm -v "$PWD":/src -w /src \
        -v orca-cargo-registry:/usr/local/cargo/registry \
        rust:bookworm cargo build --release --target-dir /src/target-release-container
    cp target-release-container/release/orca "$DIST/orca"
else
    echo "==> docker not found; building on host (binary needs host glibc or newer)"
    cargo build --release
    cp target/release/orca "$DIST/orca"
fi

cp -r plasmoid "$DIST/plasmoid"
mkdir -p "$DIST/packaging" "$DIST/assets"
cp packaging/orca.desktop "$DIST/packaging/"
cp assets/orca-launcher.png "$DIST/assets/"
tar -C "$DIST" -czf "$DIST/orca-linux-$ARCH.tar.gz" orca plasmoid packaging assets
echo "==> Asset: $DIST/orca-linux-$ARCH.tar.gz"

gh release create "$TAG" "$DIST/orca-linux-$ARCH.tar.gz" \
    --title "Orca for Linux $TAG" \
    --notes "Tray status tracker for CLI AI agents — Linux port of Orca.

Quick install:
\`\`\`sh
curl -fsSL https://raw.githubusercontent.com/FatihErtugral/orca-linux/master/install.sh | bash
\`\`\`"
echo "==> Released $TAG"

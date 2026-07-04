CARGO ?= cargo
PREFIX ?= $(HOME)/.local

.PHONY: build release test lint fmt install install-hooks clean

build:
	$(CARGO) build

release:
	$(CARGO) build --release

test:
	$(CARGO) test

lint:
	$(CARGO) clippy -- -D warnings

fmt:
	$(CARGO) fmt

install: release
	install -Dm755 target/release/orca $(PREFIX)/bin/orca
	@echo "installed -> $(PREFIX)/bin/orca"

install-hooks: install
	$(PREFIX)/bin/orca install-hooks

install-plasmoid:
	kpackagetool6 --type Plasma/Applet --install plasmoid 2>/dev/null || \
	kpackagetool6 --type Plasma/Applet --upgrade plasmoid

install-desktop:
	install -Dm644 packaging/orca.desktop $(HOME)/.local/share/applications/orca.desktop
	install -Dm644 assets/orca-launcher.png $(HOME)/.local/share/icons/hicolor/256x256/apps/orca.png
	-kbuildsycoca6 2>/dev/null

clean:
	$(CARGO) clean

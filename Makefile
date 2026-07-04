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

clean:
	$(CARGO) clean

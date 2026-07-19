# solstone-linux Makefile
# Standalone Linux desktop observer for solstone

.PHONY: all bootstrap install format test ci audit update-deps shellcheck install-service uninstall-service service-restart service-status service-logs versions clean clean-install release legacy-python-bootstrap legacy-python-install legacy-python-format legacy-python-test legacy-python-test-only legacy-python-ci legacy-python-release legacy-python-release-test check-toolchain-env establish-toolchain rust-preflight check-cargo-deny

APP := solstone-linux
UNIT := solstone-linux.service
CARGO ?= cargo
RUSTUP ?= rustup
RUST_VERSION := $(shell sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml 2>/dev/null)
AMBIENT_RUSTUP_TOOLCHAIN := $(RUSTUP_TOOLCHAIN)
export RUSTUP_TOOLCHAIN := $(RUST_VERSION)
RUST_TARGET := x86_64-unknown-linux-gnu
CARGO_LOCKED := --locked
CARGO_DENY_VERSION := 0.20.2
CARGO_DEB_VERSION := 3.7.0
CARGO_GENERATE_RPM_VERSION := 0.21.0
SHELLCHECK_SCRIPTS := scripts/build-release.sh scripts/install.sh

VENV := .venv
VENV_BIN := $(VENV)/bin
PYTHON := $(VENV_BIN)/python
PYTEST := $(VENV_BIN)/pytest
RUFF := $(VENV_BIN)/ruff
UV := $(shell command -v uv 2>/dev/null)
PIPX_FLAGS := --system-site-packages
VENV_FLAGS := --system-site-packages

all: install

check-toolchain-env:
	@test -n "$(RUST_VERSION)" || { echo "error: rust-toolchain.toml is missing or has no channel" >&2; exit 1; }
	@if [ -n "$(AMBIENT_RUSTUP_TOOLCHAIN)" ] && [ "$(AMBIENT_RUSTUP_TOOLCHAIN)" != "$(RUST_VERSION)" ]; then \
		echo "error: Rust toolchain mismatch: expected $(RUST_VERSION), RUSTUP_TOOLCHAIN is '$(AMBIENT_RUSTUP_TOOLCHAIN)'" >&2; \
		echo "repair: unset RUSTUP_TOOLCHAIN" >&2; \
		echo "repair: rustup toolchain install $(RUST_VERSION) --component rustfmt --component clippy" >&2; \
		exit 1; \
	fi

establish-toolchain: check-toolchain-env
	@command -v $(RUSTUP) >/dev/null 2>&1 || { echo "error: rustup not found; run 'make bootstrap'" >&2; exit 1; }
	$(RUSTUP) toolchain install $(RUST_VERSION) --profile minimal --component rustfmt --component clippy --target $(RUST_TARGET)

rust-preflight: check-toolchain-env
	@actual=$$($(CARGO) --version >/dev/null 2>&1 && rustc --version --verbose | sed -n 's/^release: //p'); \
	if [ "$$actual" != "$(RUST_VERSION)" ]; then \
		echo "error: Rust toolchain mismatch: expected $(RUST_VERSION), actual $${actual:-unavailable}" >&2; \
		echo "repair: rustup toolchain install $(RUST_VERSION) --component rustfmt --component clippy" >&2; \
		exit 1; \
	fi

check-cargo-deny:
	@actual=$$(cargo deny --version 2>/dev/null || true); \
	if [ -z "$$actual" ]; then \
		echo "error: cargo-deny not found; expected 'cargo-deny $(CARGO_DENY_VERSION)'; run 'make install'" >&2; exit 1; \
	elif [ "$$actual" != "cargo-deny $(CARGO_DENY_VERSION)" ]; then \
		echo "error: cargo-deny version mismatch: expected 'cargo-deny $(CARGO_DENY_VERSION)', got '$$actual'; run 'make install'" >&2; exit 1; \
	fi

install: establish-toolchain rust-preflight
	@actual=$$(cargo deny --version 2>/dev/null || true); \
	if [ "$$actual" != "cargo-deny $(CARGO_DENY_VERSION)" ]; then \
		$(CARGO) install cargo-deny --version $(CARGO_DENY_VERSION) $(CARGO_LOCKED) || { echo "error: cargo-deny $(CARGO_DENY_VERSION) tool not established; cargo install failed" >&2; exit 1; }; \
	fi
	@actual=$$(cargo deny --version 2>/dev/null || true); \
	[ "$$actual" = "cargo-deny $(CARGO_DENY_VERSION)" ] || { echo "error: cargo-deny $(CARGO_DENY_VERSION) tool not established; got '$$actual'" >&2; exit 1; }
	$(CARGO) install --path crates/solstone-linux $(CARGO_LOCKED)

bootstrap:
	@if ! command -v rustup >/dev/null 2>&1; then curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path; fi
	@$(MAKE) install

format: rust-preflight
	$(CARGO) fmt

test: rust-preflight
	$(CARGO) test $(CARGO_LOCKED) -p solstone-linux

shellcheck:
	shellcheck $(SHELLCHECK_SCRIPTS)

ci: rust-preflight check-cargo-deny
	@echo "Evidence class: host evidence (format, lint, tests, and offline dependency policy)."
	@echo "This gate does not run target-package validation or the release FLAC soak."
	$(CARGO) fmt --check
	$(CARGO) clippy $(CARGO_LOCKED) --all-targets -- -D warnings
	$(CARGO) test $(CARGO_LOCKED) -p solstone-linux
	$(MAKE) shellcheck
	cargo deny $(CARGO_LOCKED) --offline check licenses bans sources

audit: rust-preflight check-cargo-deny
	@echo "Evidence class: refreshed advisory evidence."
	cargo deny fetch db
	cargo deny $(CARGO_LOCKED) check advisories

update-deps: rust-preflight
	$(CARGO) update

install-service: install
	$(APP) install-service

uninstall-service: rust-preflight
	$(CARGO) run $(CARGO_LOCKED) -p solstone-linux -- uninstall-service

service-restart:
	systemctl --user restart $(UNIT)

service-status:
	systemctl --user --no-pager status $(UNIT)

service-logs:
	journalctl --user -u $(UNIT) -n 100 --no-pager -f

versions: rust-preflight check-cargo-deny
	rustc --version --verbose
	$(CARGO) --version
	cargo deny --version
	@command -v $(APP) >/dev/null 2>&1 && $(APP) --version || true

release: rust-preflight
	@echo "Evidence class: target-package drift evidence. This does not run the release FLAC soak."
	@bash scripts/build-release.sh deb
	@bash scripts/build-release.sh rpm

clean:
	@echo "Cleaning build artifacts and cache files..."
	rm -rf build/ dist/ *.egg-info/
	rm -rf target/
	rm -rf .pytest_cache/ .mypy_cache/
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	find . -type f -name "*.pyo" -delete
	rm -f .legacy-python-installed
	rm -rf $(VENV)

clean-install: clean install

.legacy-python-installed: pyproject.toml
	@command -v uv >/dev/null 2>&1 || { echo "error: uv is required for legacy Python targets" >&2; exit 1; }
	@[ -f $(VENV)/pyvenv.cfg ] || $(UV) venv $(VENV_FLAGS) --python /usr/bin/python3 $(VENV)
	$(UV) sync --group dev --no-install-package pygobject --no-install-package pycairo
	@touch .legacy-python-installed

legacy-python-install: .legacy-python-installed

legacy-python-format: .legacy-python-installed
	$(RUFF) format .
	$(RUFF) check --fix .

legacy-python-test: .legacy-python-installed
	$(PYTEST) tests/ -q

legacy-python-test-only: .legacy-python-installed
	@test -n "$(TEST)" || { echo "Usage: make legacy-python-test-only TEST=<test_file_or_pattern>" >&2; exit 1; }
	$(PYTEST) $(TEST)

legacy-python-ci: .legacy-python-installed
	$(RUFF) format --check .
	$(RUFF) check .
	$(PYTEST) tests/ -q

legacy-python-bootstrap:
	@if command -v uv >/dev/null 2>&1; then echo "uv already installed"; else curl -LsSf https://astral.sh/uv/install.sh | sh; fi
	@$(MAKE) legacy-python-install

legacy-python-release:
	@bash scripts/release.sh

legacy-python-release-test:
	@bash scripts/release.sh --test

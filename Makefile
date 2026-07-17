# solstone-linux Makefile
# Standalone Linux desktop observer for solstone

.PHONY: install test test-only format ci rust-fmt-check rust-lint rust-test rust-deny clean clean-install versions all bootstrap install-service service-restart service-status service-logs uninstall-service release release-test

# Default target
all: install

# Virtual environment directory
VENV := .venv
VENV_BIN := $(VENV)/bin
PYTHON := $(VENV_BIN)/python
CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)
CARGO_DENY ?= $(shell command -v cargo-deny 2>/dev/null || { [ -x $(HOME)/.cargo/bin/cargo-deny ] && echo $(HOME)/.cargo/bin/cargo-deny; })

# Require uv
UV := $(shell command -v uv 2>/dev/null)
ifneq ($(filter bootstrap,$(MAKECMDGOALS)),bootstrap)
ifndef UV
$(error uv is not installed. Run: make bootstrap)
endif
endif

APP := solstone-linux
UNIT := solstone-linux.service
PIPX_FLAGS := --system-site-packages
VENV_FLAGS := --system-site-packages

# Marker file to track installation
.installed: pyproject.toml
	@echo "Installing package with uv (including dev tools)..."
	@[ -f $(VENV)/pyvenv.cfg ] || $(UV) venv $(VENV_FLAGS) --python /usr/bin/python3 $(VENV)
	$(UV) sync --group dev --no-install-package pygobject --no-install-package pycairo
	@touch .installed

# Install package in editable mode with isolated venv
install: .installed

bootstrap:
	@if command -v uv >/dev/null 2>&1; then \
		echo "uv already installed"; \
	else \
		echo "installing uv..."; \
		curl -LsSf https://astral.sh/uv/install.sh | sh; \
	fi
	@if ! command -v pipx >/dev/null 2>&1; then \
		echo "pipx missing — install instructions:"; \
		echo "  fedora:   sudo dnf install pipx"; \
		echo "  debian:   sudo apt install pipx"; \
		echo "  arch:     sudo pacman -S python-pipx"; \
		echo "  opensuse: sudo zypper install python3-pipx"; \
		exit 1; \
	fi
	@python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3,10) else 1)' || { \
		echo "python >=3.10 required"; exit 1; \
	}
	@if [ -f .installed ]; then \
		$(VENV_BIN)/solstone-linux doctor; \
	else \
		echo "now run: make install-service"; \
	fi

install-service: .installed
	@$(VENV_BIN)/solstone-linux doctor
	@command -v pipx >/dev/null || { echo "pipx not found — install with: sudo dnf install pipx (or apt/brew equivalent)"; exit 1; }
	@$(PYTHON) -m solstone_linux.install_guard preinstall "$(CURDIR)"; rc=$$?; \
	 if [ $$rc -eq 2 ]; then exit 1; \
	 elif [ $$rc -eq 10 ]; then $(MAKE) ci; \
	 fi
	# Editable installs (pipx install -e .) are deliberately avoided: pipx treats editable installs differently and system-site-packages behavior is unreliable with them.
	pipx install --force $(PIPX_FLAGS) .
	$(PYTHON) -m solstone_linux.install_guard write "$(CURDIR)"
	$(APP) install-service
	systemctl --user status $(UNIT) --no-pager -l | head -n 20 || true

service-restart:
	systemctl --user restart $(UNIT)

service-status:
	systemctl --user --no-pager status $(UNIT)

service-logs:
	journalctl --user -u $(UNIT) -n 100 --no-pager -f

uninstall-service: .installed
	@$(PYTHON) -m solstone_linux.install_guard preuninstall "$(CURDIR)"; rc=$$?; \
	 if [ $$rc -eq 2 ]; then exit 1; \
	 elif [ $$rc -eq 0 ]; then exit 0; \
	 fi
	-systemctl --user stop $(UNIT)
	-systemctl --user disable $(UNIT)
	-rm -f $(HOME)/.config/systemd/user/$(UNIT)
	-systemctl --user daemon-reload
	-pipx uninstall $(APP)
	$(PYTHON) -m solstone_linux.install_guard remove

# Venv tool shortcuts
PYTEST := $(VENV_BIN)/pytest
RUFF := $(VENV_BIN)/ruff

# Run all tests
test: .installed
	@echo "Running tests..."
	$(PYTEST) tests/ -q

# Run specific test file or pattern
test-only: .installed
	@if [ -z "$(TEST)" ]; then \
		echo "Usage: make test-only TEST=<test_file_or_pattern>"; \
		echo "Example: make test-only TEST=tests/test_config.py"; \
		echo "Example: make test-only TEST=\"-k test_function_name\""; \
		exit 1; \
	fi
	$(PYTEST) $(TEST)

# Auto-format and fix code, then report remaining issues
format: .installed
	@echo "Formatting and fixing code with ruff..."
	@$(RUFF) format .
	@$(RUFF) check --fix .
	@echo ""
	@echo "Checking for remaining issues..."
	@$(RUFF) check . || { echo ""; echo "Issues above need manual fixes."; exit 1; }
	@echo ""
	@echo "All clean!"

rust-fmt-check:
	$(CARGO) fmt --check

rust-lint:
	$(CARGO) clippy --all-targets -- -D warnings

rust-test:
	$(CARGO) test

rust-deny:
	@if [ -n "$(CARGO_DENY)" ] && [ -x "$(CARGO_DENY)" ]; then \
		echo "Running cargo deny check with $(CARGO_DENY)..."; \
		"$(CARGO_DENY)" check; \
	else \
		echo "NOTICE: cargo-deny not found on PATH or at $(HOME)/.cargo/bin/cargo-deny; skipping cargo deny check"; \
	fi

# Run CI checks (what CI would run)
ci: .installed
	@echo "Running CI checks..."
	@echo "=== Checking formatting ==="
	@$(RUFF) format --check . || { echo "Run 'make format' to fix formatting"; exit 1; }
	@echo ""
	@echo "=== Running ruff ==="
	@$(RUFF) check . || { echo "Run 'make format' to auto-fix"; exit 1; }
	@echo ""
	@echo "=== Running tests ==="
	@$(MAKE) test
	@echo ""
	@echo "=== Checking Rust formatting ==="
	@$(MAKE) rust-fmt-check
	@echo ""
	@echo "=== Running Rust clippy ==="
	@$(MAKE) rust-lint
	@echo ""
	@echo "=== Running Rust tests ==="
	@$(MAKE) rust-test
	@echo ""
	@echo "=== Checking Rust dependencies ==="
	@$(MAKE) rust-deny
	@echo ""
	@echo "All CI checks passed!"

# Clean build artifacts and cache files
clean:
	@echo "Cleaning build artifacts and cache files..."
	rm -rf build/ dist/ *.egg-info/
	rm -rf target/
	rm -rf .pytest_cache/ .mypy_cache/
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	find . -type f -name "*.pyo" -delete
	rm -f .installed
	rm -rf $(VENV)

# Clean everything and reinstall
clean-install: clean install

# Show installed package versions
versions: .installed
	@echo "=== Python version ==="
	$(PYTHON) --version
	@echo ""
	@echo "=== Installed packages ==="
	@$(UV) pip list | grep -E "^(pytest|ruff|requests|numpy|soundfile|soundcard|dbus-fast|PyGObject)" || true

release: ## Publish solstone-linux to PyPI (production)
	@bash scripts/release.sh

release-test: ## Publish solstone-linux to TestPyPI
	@bash scripts/release.sh --test

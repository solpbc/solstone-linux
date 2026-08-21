# solstone-linux Makefile
# Standalone Linux desktop observer for solstone

.PHONY: all bootstrap install format brand-sync test check-observer-contract check-rust-release-manifest check-transparency-minisign check-audit-signed-packet ci audit update-deps shellcheck install-service uninstall-service service-restart service-status service-logs versions clean clean-install release release-images release-candidate release-candidate-prove release-candidate-recover publish-release publish-transparency resign-transparency-pointer check-toolchain-env establish-toolchain rust-preflight check-cargo-deny

APP := solstone-linux
UNIT := solstone-linux.service
CARGO ?= cargo
RUSTUP ?= rustup
CARGO_HOME ?= $(HOME)/.cargo
CARGO_BIN_DIR := $(CARGO_HOME)/bin
RUST_VERSION := $(shell sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml 2>/dev/null)
AMBIENT_RUSTUP_TOOLCHAIN := $(RUSTUP_TOOLCHAIN)
export RUSTUP_TOOLCHAIN := $(RUST_VERSION)
export PATH := $(CARGO_BIN_DIR):$(PATH)
RUST_TARGET := x86_64-unknown-linux-gnu
CARGO_LOCKED := --locked
CARGO_DENY_VERSION := 0.20.2
CARGO_DEB_VERSION := 3.7.0
CARGO_GENERATE_RPM_VERSION := 0.21.0
# Transparency is paused during the Rust conversion freeze. Restore only after
# the post-conversion review by changing this checked-in activation switch to 1.
TRANSPARENCY_ACTIVATED ?= 0
# Proof roles are provisioned images now, so keep their immutable stock bases explicit.
UBUNTU_STOCK_BASE := sha256:b8e6b596a32475661d9fcaf4a212fcc7736e0d8d1494973aefdbcc71c442d890
FEDORA_STOCK_BASE := sha256:8c219b734f781909b9384edc01eb52318330b57fa58e0410dfcf973b01d28fcd
SHELLCHECK_SCRIPTS := scripts/build-release.sh scripts/extract_changelog.sh scripts/install.sh scripts/publish-release.sh

all: install

check-toolchain-env:
	@test -n "$(RUST_VERSION)" || { echo "error: Rust toolchain declaration mismatch: expected a channel in rust-toolchain.toml, actual missing or malformed" >&2; echo "repair: git restore rust-toolchain.toml" >&2; exit 1; }
	@if [ -n "$(AMBIENT_RUSTUP_TOOLCHAIN)" ] && [ "$(AMBIENT_RUSTUP_TOOLCHAIN)" != "$(RUST_VERSION)" ]; then \
		echo "error: Rust toolchain mismatch: expected $(RUST_VERSION), RUSTUP_TOOLCHAIN is '$(AMBIENT_RUSTUP_TOOLCHAIN)'" >&2; \
		echo "repair: unset RUSTUP_TOOLCHAIN" >&2; \
		echo "repair: rustup toolchain install $(RUST_VERSION) --component rustfmt --component clippy" >&2; \
		exit 1; \
	fi

establish-toolchain: check-toolchain-env
	@command -v $(RUSTUP) >/dev/null 2>&1 || { echo "error: Rust installer mismatch: expected rustup on PATH, actual not found" >&2; echo "repair: make bootstrap" >&2; exit 1; }
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
		$(CARGO) install cargo-deny --version $(CARGO_DENY_VERSION) $(CARGO_LOCKED) || { actual=$$(cargo deny --version 2>/dev/null || echo unavailable); echo "error: cargo-deny installation mismatch: expected 'cargo-deny $(CARGO_DENY_VERSION)', actual '$$actual'" >&2; echo "repair: cargo install cargo-deny --version $(CARGO_DENY_VERSION) --locked" >&2; exit 1; }; \
	fi
	@actual=$$(cargo deny --version 2>/dev/null || true); \
	[ "$$actual" = "cargo-deny $(CARGO_DENY_VERSION)" ] || { echo "error: cargo-deny verification mismatch: expected 'cargo-deny $(CARGO_DENY_VERSION)', actual '$${actual:-unavailable}'" >&2; echo "repair: cargo install cargo-deny --version $(CARGO_DENY_VERSION) --locked" >&2; exit 1; }
	@$(CARGO) install --path crates/solstone-linux $(CARGO_LOCKED) || { echo "error: observer installation mismatch: expected $(CARGO_BIN_DIR)/$(APP), actual installation failed" >&2; echo "repair: cargo install --path crates/solstone-linux --locked" >&2; exit 1; }

bootstrap:
	@if ! command -v rustup >/dev/null 2>&1; then curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path; fi
	@$(MAKE) install

format: rust-preflight
	$(CARGO) fmt

# Re-vendor brand assets from the canonical brand source. CI verifies the
# committed output (it does not run brand-sync) — run this locally when the
# brand spec updates, then commit the diff.
#
# The scalable sources are copies. The hicolor PNG ladder has no committed
# raster in the brand source: each size is rendered straight from the vendored
# app SVG at its exact pixel size (never downsampled from one raster), which
# needs rsvg-convert (librsvg).
#   apt: librsvg2-bin   dnf: librsvg2-tools
#
# The status names are this theme's, not the brand source's — the mapping below
# is the contract crates/solstone-linux/build.rs embeds by constant.
BRAND_ICON_DIR    = contrib/icons/hicolor
BRAND_STATUS_SYNC = solstone-recording:mark solstone-paused:mark-paused solstone-syncing:mark-connecting solstone-error:mark-error solstone-attention:mark-attention solstone-offline:mark-offline
BRAND_ICON_SIZES  = 16 24 32 48 64 128 256 512

brand-sync:
	@test -n "$(BRAND_DIR)" || { echo "brand: BRAND_DIR is required — point it at your brand asset directory (BRAND_DIR=/path/to/brand make brand-sync)"; exit 1; }
	@test -d "$(BRAND_DIR)" || { echo "brand: BRAND_DIR=$(BRAND_DIR) not found"; exit 1; }
	@command -v rsvg-convert >/dev/null 2>&1 || { echo "brand: rsvg-convert (librsvg) not found — apt install librsvg2-bin, or dnf install librsvg2-tools"; exit 1; }
	@set -e; for pair in $(BRAND_STATUS_SYNC); do \
	  cp "$(BRAND_DIR)/$${pair#*:}.svg" "$(BRAND_ICON_DIR)/scalable/status/$${pair%%:*}.svg"; \
	done
	cp "$(BRAND_DIR)/app-icon/app-icon-transparent.svg" $(BRAND_ICON_DIR)/scalable/apps/solstone-observer.svg
	@set -e; for size in $(BRAND_ICON_SIZES); do \
	  rsvg-convert -w $$size -h $$size $(BRAND_ICON_DIR)/scalable/apps/solstone-observer.svg \
	    -o $(BRAND_ICON_DIR)/$${size}x$${size}/apps/solstone-observer.png; \
	done
	@echo "brand: synced from $(BRAND_DIR)"

test: rust-preflight
	$(CARGO) test $(CARGO_LOCKED) -p solstone-linux

check-observer-contract: rust-preflight
	@echo "Observer contract bundle: 9.0.0"
	@echo "Observer contract manifest SHA-256: 93b2a5a1604f1ba6fad30624c00cac98ea3d04a80cb1718886cf665c16f58834"
	@inventory=$$(CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p solstone-linux observer_contract_tests:: -- --list); \
	printf '%s\n' "$$inventory"; \
	printf '%s\n' "$$inventory" | grep -Fx 'observer_contract_tests::observer_contract_conformance: test' >/dev/null || { echo "error: observer contract test inventory mismatch" >&2; exit 1; }; \
	actual=$$(printf '%s\n' "$$inventory" | grep -c '^observer_contract_tests::'); \
	[ "$$actual" -eq 1 ] || { echo "error: observer contract test inventory mismatch: expected 1, actual $$actual" >&2; exit 1; }
	@output=$$(mktemp); \
	trap 'rm -f "$$output"' EXIT; \
	CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p solstone-linux observer_contract_tests::observer_contract_conformance -- --exact >"$$output" 2>&1 || { status=$$?; tail -50 "$$output"; exit $$status; }; \
	tail -50 "$$output"; \
	grep -Eq 'test result: ok\. 1 passed; 0 failed' "$$output" || { echo "error: observer contract named test did not execute" >&2; exit 1; }

check-package-audit: rust-preflight
	@test -n "$(strip $(TAR))" || { echo "error: TAR is required" >&2; exit 1; }
	@test -n "$(strip $(DEB))" || { echo "error: DEB is required" >&2; exit 1; }
	@test -n "$(strip $(RPM))" || { echo "error: RPM is required" >&2; exit 1; }
	@test -n "$(strip $(EXPECTED_EXECUTABLE_SHA256))" || { echo "error: EXPECTED_EXECUTABLE_SHA256 is required" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- audit-packages \
		--tar "$(TAR)" \
		--deb "$(DEB)" \
		--rpm "$(RPM)" \
		--expected-executable-sha256 "$(EXPECTED_EXECUTABLE_SHA256)"

check-rust-release-manifest: rust-preflight
	@echo "Rust release manifest schema: 1"
	@echo "Rust release manifest schema SHA-256: d4eabf52bcc68b56945912d351f818e5444fe8c6461cb5c48b096f87b17a875c"
	@echo "Rust release candidate ledger schema: 1"
	@echo "Rust release candidate ledger schema SHA-256: 4b387f19d8018752c6d016a4c0c74343ed80d2b64a3ff9480aa75b04fa66882d"
	@echo "Rust release candidate proof schema: 1"
	@echo "Rust release candidate proof schema SHA-256: 3009eab983eea832961220406f19c7459ed1db7fffc352af6ffaf664f9cd7dcf"
	@manifest_set=$(if $(filter environment%,$(origin MANIFEST)),1,$(if $(findstring command line,$(origin MANIFEST)),1,0)); \
	release_dir_set=$(if $(filter environment%,$(origin RELEASE_DIR)),1,$(if $(findstring command line,$(origin RELEASE_DIR)),1,0)); \
	if { [ "$$manifest_set" -eq 1 ] && [ -z "$(strip $(MANIFEST))" ]; } || { [ "$$release_dir_set" -eq 1 ] && [ -z "$(strip $(RELEASE_DIR))" ]; }; then echo "error: release manifest selector empty" >&2; exit 1; \
	elif [ "$$manifest_set" -eq 1 ] && [ "$$release_dir_set" -eq 1 ]; then echo "error: release manifest mode mismatch: expected one selector, actual two" >&2; exit 1; \
	elif [ "$$manifest_set" -eq 1 ]; then \
		CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- validate --manifest "$(MANIFEST)"; \
	elif [ "$$release_dir_set" -eq 1 ]; then \
		CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- validate --release-dir "$(RELEASE_DIR)"; \
	else \
		inventory=$$(CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p rust-release-manifest tests::rust_release_manifest_conformance -- --list); \
		printf '%s\n' "$$inventory"; \
		printf '%s\n' "$$inventory" | grep -Fx 'tests::rust_release_manifest_conformance: test' >/dev/null || { echo "error: release manifest test inventory mismatch" >&2; exit 1; }; \
		actual=$$(printf '%s\n' "$$inventory" | grep -c '^tests::rust_release_manifest_conformance:'); \
		[ "$$actual" -eq 1 ] || { echo "error: release manifest test inventory mismatch: expected 1, actual $$actual" >&2; exit 1; }; \
		output=$$(mktemp); \
		trap 'rm -f "$$output"' EXIT; \
		CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p rust-release-manifest -- --skip transparency >"$$output" 2>&1 || { status=$$?; tail -50 "$$output"; exit $$status; }; \
		tail -50 "$$output"; \
		results=$$(grep '^test result:' "$$output" || true); \
		[ -n "$$results" ] && ! printf '%s\n' "$$results" | grep -Ev '^test result: ok\..*0 failed' >/dev/null && ! grep -F 'FAILED' "$$output" >/dev/null || { echo "error: release manifest test suite did not pass" >&2; exit 1; }; \
	fi

shellcheck:
	shellcheck $(SHELLCHECK_SCRIPTS)

ci: rust-preflight check-cargo-deny check-observer-contract check-rust-release-manifest check-audit-signed-packet
	@echo "Evidence class: host evidence (format, lint, tests, and offline dependency policy)."
	@echo "This gate does not run target-package validation or the release FLAC soak."
	$(CARGO) fmt --check
	$(CARGO) clippy $(CARGO_LOCKED) --all-targets -- -D warnings
	$(CARGO) test $(CARGO_LOCKED) -p solstone-linux
	$(MAKE) shellcheck
	cargo deny $(CARGO_LOCKED) --offline check licenses bans sources

check-transparency-minisign:
	@test "$(TRANSPARENCY_ACTIVATED)" = 1 || { echo "transparency is suspended for the Rust conversion freeze; restore with TRANSPARENCY_ACTIVATED=1 only after the post-conversion review" >&2; exit 2; }
	@$(MAKE) rust-preflight
	@command -v minisign >/dev/null 2>&1 || { echo "error: minisign prerequisite mismatch: expected minisign on PATH, actual missing" >&2; echo "repair: sudo zypper install minisign" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p rust-release-manifest transparency_tests::real_minisign_sign_verify_and_reject_tamper -- --exact --ignored

check-audit-signed-packet: rust-preflight check-cargo-deny
	@command -v minisign >/dev/null 2>&1 || { echo "error: minisign prerequisite mismatch: expected minisign on PATH, actual missing" >&2; echo "repair: sudo zypper install minisign" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) test $(CARGO_LOCKED) -p rust-release-manifest audit_tests::real_signed_packet_local_audit -- --exact --ignored

publish-transparency:
	@test "$(TRANSPARENCY_ACTIVATED)" = 1 || { echo "transparency is suspended for the Rust conversion freeze; restore with TRANSPARENCY_ACTIVATED=1 only after the post-conversion review" >&2; exit 2; }
	@$(MAKE) rust-preflight
	@test -n "$(strip $(RELEASE_DIR))" || { echo "error: transparency release directory mismatch: expected RELEASE_DIR, actual missing" >&2; echo "repair: make publish-transparency RELEASE_DIR=<retained-candidate>" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- transparency publish --release-dir "$(RELEASE_DIR)"

publish-release: rust-preflight
	@test -n "$(strip $(RELEASE_DIR))" || { echo "error: release directory mismatch: expected RELEASE_DIR, actual missing" >&2; echo "repair: make publish-release RELEASE_DIR=dist/rust" >&2; exit 1; }
	bash scripts/publish-release.sh "$(RELEASE_DIR)"

resign-transparency-pointer:
	@test "$(TRANSPARENCY_ACTIVATED)" = 1 || { echo "transparency is suspended for the Rust conversion freeze; restore with TRANSPARENCY_ACTIVATED=1 only after the post-conversion review" >&2; exit 2; }
	@$(MAKE) rust-preflight
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- transparency resign-pointer

audit: rust-preflight check-cargo-deny
	@test -n "$(strip $(BUNDLE))" || { echo "error: audit bundle mismatch: expected BUNDLE, actual missing" >&2; echo "repair: make audit BUNDLE=<bundle> RECEIPT=<receipt> PUBKEY=<pubkey> LOCATOR=<locator>" >&2; exit 1; }
	@test -n "$(strip $(RECEIPT))" || { echo "error: audit receipt mismatch: expected RECEIPT, actual missing" >&2; echo "repair: make audit BUNDLE=<bundle> RECEIPT=<receipt> PUBKEY=<pubkey> LOCATOR=<locator>" >&2; exit 1; }
	@test -n "$(strip $(PUBKEY))" || { echo "error: audit public key mismatch: expected PUBKEY, actual missing" >&2; echo "repair: make audit BUNDLE=<bundle> RECEIPT=<receipt> PUBKEY=<pubkey> LOCATOR=<locator>" >&2; exit 1; }
	@test -n "$(strip $(LOCATOR))" || { echo "error: audit locator mismatch: expected LOCATOR, actual missing" >&2; echo "repair: make audit BUNDLE=<bundle> RECEIPT=<receipt> PUBKEY=<pubkey> LOCATOR=<locator>" >&2; exit 1; }
	@CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- audit --bundle "$(BUNDLE)" --receipt "$(RECEIPT)" --pubkey "$(PUBKEY)" --locator "$(LOCATOR)"

update-deps: rust-preflight
	$(CARGO) update

install-service: install
	$(CARGO_BIN_DIR)/$(APP) install-service

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

release: release-candidate

release-images:
	@# Build and proof images share the same explicitly pinned stock bases.
	podman build --pull=never --no-cache --file packaging/Containerfile.tools --target ubuntu-tools --tag localhost/solstone-linux-build-ubuntu \
		--build-arg "UBUNTU_STOCK_BASE=$(UBUNTU_STOCK_BASE)" --build-arg "FEDORA_STOCK_BASE=$(FEDORA_STOCK_BASE)" \
		--build-arg "RUST_VERSION=$(RUST_VERSION)" --build-arg "CARGO_DEB_VERSION=$(CARGO_DEB_VERSION)" .
	podman build --pull=never --no-cache --file packaging/Containerfile.tools --target fedora-tools --tag localhost/solstone-linux-build-fedora \
		--build-arg "UBUNTU_STOCK_BASE=$(UBUNTU_STOCK_BASE)" --build-arg "FEDORA_STOCK_BASE=$(FEDORA_STOCK_BASE)" \
		--build-arg "RUST_VERSION=$(RUST_VERSION)" --build-arg "CARGO_GENERATE_RPM_VERSION=$(CARGO_GENERATE_RPM_VERSION)" .
	podman build --pull=never --no-cache --file packaging/Containerfile.tools --target ubuntu-proof --tag localhost/solstone-linux-proof-ubuntu \
		--build-arg "UBUNTU_STOCK_BASE=$(UBUNTU_STOCK_BASE)" --build-arg "FEDORA_STOCK_BASE=$(FEDORA_STOCK_BASE)" .
	podman build --pull=never --no-cache --file packaging/Containerfile.tools --target fedora-proof --tag localhost/solstone-linux-proof-fedora \
		--build-arg "UBUNTU_STOCK_BASE=$(UBUNTU_STOCK_BASE)" --build-arg "FEDORA_STOCK_BASE=$(FEDORA_STOCK_BASE)" .

release-candidate: rust-preflight
	@test -n "$(strip $(EXPECTED_RELEASE_COMMIT))" || { echo "error: expected release commit mismatch: expected EXPECTED_RELEASE_COMMIT, actual missing" >&2; echo "repair: make release-candidate EXPECTED_RELEASE_COMMIT=<full-commit> ADVISORY_DESCRIPTOR=<descriptor.json>" >&2; exit 1; }
	@test -n "$(strip $(ADVISORY_DESCRIPTOR))" || { echo "error: advisory descriptor mismatch: expected ADVISORY_DESCRIPTOR, actual missing" >&2; echo "repair: make release-candidate EXPECTED_RELEASE_COMMIT=$(EXPECTED_RELEASE_COMMIT) ADVISORY_DESCRIPTOR=<descriptor.json>" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- candidate create --expected-release-commit "$(EXPECTED_RELEASE_COMMIT)" --advisory-descriptor "$(ADVISORY_DESCRIPTOR)"

release-candidate-prove: rust-preflight
	@test -n "$(strip $(VERSION))" || { echo "error: candidate version mismatch: expected VERSION, actual missing" >&2; echo "repair: make release-candidate-prove VERSION=<version> ADVISORY_DESCRIPTOR=<descriptor.json>" >&2; exit 1; }
	@test -n "$(strip $(ADVISORY_DESCRIPTOR))" || { echo "error: advisory descriptor mismatch: expected ADVISORY_DESCRIPTOR, actual missing" >&2; echo "repair: make release-candidate-prove VERSION=$(VERSION) ADVISORY_DESCRIPTOR=<descriptor.json>" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- candidate prove --version "$(VERSION)" --advisory-descriptor "$(ADVISORY_DESCRIPTOR)"

release-candidate-recover: rust-preflight
	@test -n "$(strip $(VERSION))" || { echo "error: candidate version mismatch: expected VERSION, actual missing" >&2; echo "repair: make release-candidate-recover VERSION=<version>" >&2; exit 1; }
	CARGO_NET_OFFLINE=true $(CARGO) run $(CARGO_LOCKED) -p rust-release-manifest -- candidate recover --version "$(VERSION)"

clean:
	@echo "Cleaning build artifacts and cache files..."
	rm -rf build/ dist/
	rm -rf target/

clean-install: clean install

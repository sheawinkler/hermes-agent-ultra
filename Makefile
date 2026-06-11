# Hermes Agent Ultra — common dev & ops targets
#
# Usage:
#   make              # show help
#   make build        # debug build (workspace)
#   make release      # build release binary (native)
#   make release-arm  # build release for ARM64 Linux
#   make test         # run tests
#   make check        # cargo check (fast)
#   make clippy       # clippy (all crates)

CARGO       ?= cargo
CROSS       := cross
BIN         := hermes-agent-ultra
BIN_CRATE   := hermes-cli
TARGET      ?= target
RELEASE_BIN := $(TARGET)/release/$(BIN)
DEBUG_BIN   := $(TARGET)/debug/$(BIN)

# Cross-compilation targets
ARM64_TARGET        := aarch64-unknown-linux-gnu
ARM64_RELEASE       := $(TARGET)/$(ARM64_TARGET)/release/$(BIN)
ARM64_MUSL_TARGET   := aarch64-unknown-linux-musl
ARM64_MUSL_RELEASE  := $(TARGET)/$(ARM64_MUSL_TARGET)/release/$(BIN)

# Override: make start CONFIG=path/to/config.yaml
CONFIG      ?=
CONFIG_FLAG := $(if $(CONFIG),--config $(CONFIG),)

# Release binary if built; otherwise `cargo run --`.
HERMES      = $(if $(wildcard $(RELEASE_BIN)),$(RELEASE_BIN),$(CARGO) run --bin $(BIN) --)

.PHONY: help build release release-arm release-arm64 \
        test check clippy clean

help:
	@echo "  build              Debug build (workspace)"
	@echo "  release            Release build (native)"
	@echo "  release-arm        Alias for release-arm64-musl (most portable)"
	@echo "  release-arm64      Build release for ARM64 Linux (glibc)"
	@echo "  release-arm64-musl Build release for ARM64 Linux (musl, fully static)"
	@echo "  test               Run workspace tests"
	@echo "  check              cargo check (fast, workspace)"
	@echo "  clippy             cargo clippy (all crates, -D warnings)"
	@echo ""
	@echo "  start              Run hermes (release if built, else debug)"
	@echo ""
	@echo "Options:"
	@echo "  CONFIG=path        Pass --config to hermes (e.g. CONFIG=config.yaml)"

build:
	$(CARGO) build

release:
	$(CARGO) build --release --bin $(BIN)

release-arm: release-arm64

release-arm64:
	$(CROSS) build --release --target $(ARM64_TARGET) --bin $(BIN)
	@echo "Built $(ARM64_RELEASE)"

test:
	$(CARGO) test

check:
	$(CARGO) check

clippy:
	$(CARGO) clippy -- -D warnings

start:
	$(HERMES) $(CONFIG_FLAG)

clean:
	$(CARGO) clean

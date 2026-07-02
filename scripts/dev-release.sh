#!/usr/bin/env bash
# dev-release.sh — build the deployed-daemon binaries at release optimization
# WITHOUT the shipping-only knobs that make iteration miserable.
#
# The root Cargo.toml [profile.release] carries `lto = "thin"` +
# `codegen-units = 1`: right for tagged releases (smallest/fastest binary,
# built rarely, in CI), wrong for the local edit→run loop — CGU=1
# serializes each crate's codegen and thin-LTO re-optimizes the whole graph
# at every link. Measured 2026-07-02: a one-line change in sovereign-core
# cost 7m29s under plain --release vs seconds with these overrides.
#
# Why env overrides instead of a custom [profile.release-dev]:
# llama-cpp-sys-4's build script resolves the cargo target dir by matching
# a path component against the `PROFILE` env var, which cargo reports as
# the BASE profile ("release") while a custom profile's directory is named
# after the custom profile — so ANY custom profile panics its build script
# with "not found". Env overrides keep the profile (and target/release/)
# intact, so the deployed `sovereign` symlink keeps working too.
#
# NOTE: the first run after a plain --release build rebuilds the workspace
# once (the knob change invalidates fingerprints) — and vice versa. On a
# dev box, always use this script for release builds; let CI/tags build
# plain --release.
#
# Usage:
#   scripts/dev-release.sh                 # the 4 CLI siblings + dev-tools
#   scripts/dev-release.sh -p sovereign-server   # any explicit cargo args
set -euo pipefail
cd "$(dirname "$0")/.."

export CARGO_PROFILE_RELEASE_LTO=off
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true

if [ "$#" -gt 0 ]; then
  exec cargo build --release "$@"
fi

exec cargo build --release --features sovereign-cli/dev-tools \
  -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-dev -p sovereign-cli-llm

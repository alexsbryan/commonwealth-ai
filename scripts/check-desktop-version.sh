#!/usr/bin/env bash
# check-desktop-version.sh — verify the Sovereign desktop app's version
# is consistent across the three files that must move together.
#
# Usage:
#   scripts/check-desktop-version.sh                 # just compare
#   scripts/check-desktop-version.sh 0.2.0           # also require that exact value
#
# Exits 0 on match, 1 on mismatch. Designed for pre-release use (run
# before `git tag desktop-v...`) and CI gating.
#
# Why three files? See sovereign/crates/sovereign-desktop/RELEASING.md
# §"Versioning". The desktop crate's src-tauri/Cargo.toml inherits via
# `version.workspace = true`, so the cargo-side source of truth is the
# workspace root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

CARGO_TOML="$REPO_ROOT/Cargo.toml"
TAURI_CONF="$REPO_ROOT/sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json"
PACKAGE_JSON="$REPO_ROOT/sovereign/crates/sovereign-desktop/package.json"

for f in "$CARGO_TOML" "$TAURI_CONF" "$PACKAGE_JSON"; do
    if [[ ! -f "$f" ]]; then
        echo "check-desktop-version: missing file: $f" >&2
        exit 2
    fi
done

# ── workspace.package.version from Cargo.toml ───────────────────────
# Walk the file in section-aware awk; only emit the version line that
# lives inside [workspace.package] (other [package] blocks elsewhere
# in the workspace would otherwise confuse a naive grep).
CARGO_VERSION="$(awk '
    /^\[workspace\.package\][[:space:]]*$/ { in_section = 1; next }
    /^\[/                                  { in_section = 0 }
    in_section && /^version[[:space:]]*=[[:space:]]*"/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*/, "")
        print
        exit
    }
' "$CARGO_TOML")"

if [[ -z "$CARGO_VERSION" ]]; then
    echo "check-desktop-version: could not find workspace.package.version in $CARGO_TOML" >&2
    exit 2
fi

# ── JSON files via python3 (always available on CI + dev) ───────────
read_json_version() {
    local file="$1"
    python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['version'])" "$file"
}

TAURI_VERSION="$(read_json_version "$TAURI_CONF")"
NPM_VERSION="$(read_json_version "$PACKAGE_JSON")"

# ── Report ──────────────────────────────────────────────────────────
printf "%-44s %s\n" "Cargo workspace.package.version:"            "$CARGO_VERSION"
printf "%-44s %s\n" "tauri.conf.json version:"                    "$TAURI_VERSION"
printf "%-44s %s\n" "package.json version:"                       "$NPM_VERSION"

EXPECTED="${1:-}"
mismatch=0

if [[ "$CARGO_VERSION" != "$TAURI_VERSION" ]] || [[ "$CARGO_VERSION" != "$NPM_VERSION" ]]; then
    mismatch=1
fi

if [[ -n "$EXPECTED" ]]; then
    printf "%-44s %s\n" "Expected (from CLI arg):" "$EXPECTED"
    if [[ "$CARGO_VERSION" != "$EXPECTED" ]] \
       || [[ "$TAURI_VERSION" != "$EXPECTED" ]] \
       || [[ "$NPM_VERSION"   != "$EXPECTED" ]]; then
        mismatch=1
    fi
fi

echo

if (( mismatch )); then
    echo "FAIL: desktop version is not consistent. Bump all three before tagging:"
    echo "  - Cargo.toml          → [workspace.package] version"
    echo "  - tauri.conf.json     → version"
    echo "  - package.json        → version"
    echo "Then re-run: scripts/check-desktop-version.sh${EXPECTED:+ $EXPECTED}"
    exit 1
fi

echo "OK: desktop version is consistent at $CARGO_VERSION."

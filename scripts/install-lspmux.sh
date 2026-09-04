#!/usr/bin/env bash
# install-lspmux.sh — one rust-analyzer per workspace, shared by every LSP client.
#
# Today each Claude Code session in this repo starts its own rust-analyzer
# through the rust-analyzer-lsp plugin (5-10 GB on the 54-crate workspace),
# and an editor adds another; two were resident at once on 2026-09-04, one
# left over from a session two days old.  lspmux (codeberg.org/p2502/lspmux,
# formerly ra-multiplex) shares ONE server per workspace folder across any
# number of clients over a local socket.  Clients see an ordinary binary
# called `rust-analyzer`; this script installs that binary as a shim.
#
# THE ONE THING TO KNOW BEFORE READING FURTHER.  The shim is not a wrapper
# around rust-analyzer; it is a fork in the road:
#
#     no arguments  -> lspmux client   (LSP stdio mode: share the server)
#     any arguments -> the real binary (batch mode: hand it over untouched)
#
# The second arm is why this script verifies before it wires.  The daemon's
# SCIP exporter spawns `rust-analyzer scip . --config-path … --output …`
# through the same PATH lookup, and an export that produces nothing WIPES the
# code-intel graph to zero symbols (observed 2026-07-13).  So the order is:
# install the shim somewhere off PATH, prove both arms against the real
# binary, and only then put it on PATH.
#
# Idempotent.  Safe to re-run: re-running refreshes the shim, the unit and
# the pinned PATH, and re-verifies.
#
#   scripts/install-lspmux.sh                # install + verify + wire PATH
#   scripts/install-lspmux.sh --no-path      # everything except the PATH edit
#   scripts/install-lspmux.sh --uninstall    # remove shim, unit, PATH line
#
# macOS differs in three ways and this script does not handle them; do it by
# hand and keep the shim, which is portable /bin/sh:
#   * Homebrew still publishes the tool under its OLD name — `brew install
#     ra-multiplex` — and installs a binary called `ra-multiplex`, not
#     `lspmux`.  Either build from the pinned source as below, or edit the
#     shim's `command -v lspmux` to match what brew put on PATH.
#   * There is no systemd.  Upstream ships `lspmux.plist` for launchd; put it
#     in ~/Library/LaunchAgents and `launchctl load` it.  Give it the same
#     PATH treatment this unit gets (see @SERVER_PATH@ below) — a launchd
#     agent's PATH is even barer than a systemd user unit's.
#   * rustup's shim directory is the same ~/.cargo/bin, but the shim must
#     still be ordered ahead of it, and macOS `/etc/paths.d` ordering makes
#     that easy to get wrong.  Verify with `type -a rust-analyzer` after.
set -euo pipefail

LSPMUX_VERSION=0.3.0
LSPMUX_REPO=https://codeberg.org/p2502/lspmux.git

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHIM_DIR="${LSPMUX_SHIM_DIR:-$HOME/.local/lspmux-shim}"
UNIT_DIR="$HOME/.config/systemd/user"
BASHRC="$HOME/.bashrc"
PATH_MARKER="# lspmux shim (scripts/install-lspmux.sh) — one shared rust-analyzer"

do_path=1
do_uninstall=0
for arg in "$@"; do
    case "$arg" in
        --no-path) do_path=0 ;;
        --uninstall) do_uninstall=1 ;;
        -h|--help) sed -n '2,60p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "install-lspmux: unknown argument '$arg'" >&2; exit 2 ;;
    esac
done

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }

# ── Uninstall ──────────────────────────────────────────────────────────────
if [[ "$do_uninstall" -eq 1 ]]; then
    say "Removing the lspmux shim"
    rm -f "$SHIM_DIR/rust-analyzer"
    rmdir "$SHIM_DIR" 2>/dev/null || true
    note "shim removed from $SHIM_DIR"
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user disable --now lspmux.service 2>/dev/null || true
        rm -f "$UNIT_DIR/lspmux.service"
        systemctl --user daemon-reload 2>/dev/null || true
        note "unit removed and stopped"
    fi
    if grep -qF "$PATH_MARKER" "$BASHRC" 2>/dev/null; then
        # Delete the marker comment and the export line that follows it.
        sed -i.lspmux-bak "\\|$PATH_MARKER|,+1d" "$BASHRC"
        note "PATH line removed from $BASHRC (backup: $BASHRC.lspmux-bak)"
    fi
    note "the real rust-analyzer at ~/.cargo/bin is untouched; open a new shell"
    exit 0
fi

# ── Preconditions ──────────────────────────────────────────────────────────
say "Preconditions"

command -v cargo >/dev/null 2>&1 || {
    echo "install-lspmux: cargo not found — install rustup first" >&2; exit 1; }

# The real rust-analyzer must exist BEFORE the shim shadows the name, or the
# first thing anyone learns is that their editor lost its language server.
# Resolve it the way the shim will: PATH, minus the shim's own directory.
real_ra="$(PATH="$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SHIM_DIR" | paste -sd:)" \
           command -v rust-analyzer || true)"
if [[ -z "$real_ra" ]]; then
    echo "install-lspmux: no rust-analyzer on PATH." >&2
    echo "  rustup component add rust-analyzer" >&2
    exit 1
fi
note "real rust-analyzer: $real_ra ($("$real_ra" --version))"

# ── lspmux itself, pinned ──────────────────────────────────────────────────
say "lspmux $LSPMUX_VERSION"

have_version=""
if command -v lspmux >/dev/null 2>&1; then
    have_version="$(lspmux --version 2>/dev/null | awk '{print $2}')"
fi

if [[ "$have_version" == "$LSPMUX_VERSION" ]]; then
    note "already installed at $(command -v lspmux) — nothing to build"
else
    if [[ -n "$have_version" ]]; then
        note "found $have_version, want $LSPMUX_VERSION — reinstalling"
    fi
    note "cargo install --git $LSPMUX_REPO --tag v$LSPMUX_VERSION --locked lspmux"
    # --locked: build the dependency versions upstream tested, not today's.
    # --tag: a moving branch is not a pin, and this binary sits between every
    # editor and its language server.
    cargo install --git "$LSPMUX_REPO" --tag "v$LSPMUX_VERSION" --locked lspmux
    hash -r
    note "installed $(lspmux --version) at $(command -v lspmux)"
fi

# ── The shim, installed OFF PATH ───────────────────────────────────────────
say "Shim"
mkdir -p "$SHIM_DIR"
install -m 0755 "$REPO_ROOT/scripts/lspmux-shim/rust-analyzer" "$SHIM_DIR/rust-analyzer"
note "installed $SHIM_DIR/rust-analyzer"

# ── Prove the pass-through arm BEFORE anything can reach the shim ──────────
say "Verifying the argument pass-through (before the shim goes on PATH)"
"$REPO_ROOT/scripts/lspmux-verify.py" --shim "$SHIM_DIR/rust-analyzer" --passthrough-only

# ── The server, as a user unit ─────────────────────────────────────────────
if ! command -v systemctl >/dev/null 2>&1; then
    say "No systemd — start the server yourself"
    note "run: lspmux server        (or use upstream's lspmux.plist on macOS)"
else
    say "lspmux server (systemd user unit)"
    mkdir -p "$UNIT_DIR"
    # The unit's PATH is this shell's PATH minus the shim directory. See the
    # comment on Environment=PATH in the unit template for why both halves
    # of that sentence are load-bearing.
    server_path="$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SHIM_DIR" | paste -sd:)"
    sed -e "s|@LSPMUX_BIN@|$(command -v lspmux)|g" \
        -e "s|@SHIM_DIR@|$SHIM_DIR|g" \
        -e "s|@SERVER_PATH@|$server_path|g" \
        "$REPO_ROOT/scripts/systemd/lspmux.service" > "$UNIT_DIR/lspmux.service"
    systemctl --user daemon-reload
    systemctl --user enable --now lspmux.service
    note "unit installed and started: systemctl --user status lspmux"
fi

# ── PATH ───────────────────────────────────────────────────────────────────
# Only now, with both arms proven, does the shim get to shadow the name.
if [[ "$do_path" -eq 1 ]]; then
    say "PATH"
    if grep -qF "$PATH_MARKER" "$BASHRC" 2>/dev/null; then
        note "already wired in $BASHRC"
    else
        # Appended at the END of .bashrc deliberately. ~/.cargo/env is sourced
        # partway down and PREPENDS ~/.cargo/bin, so a line placed earlier
        # would be silently overtaken by the very directory it must outrank.
        {
            printf '\n%s\n' "$PATH_MARKER"
            printf 'export PATH="%s:$PATH"\n' "${SHIM_DIR/#$HOME/\$HOME}"
        } >> "$BASHRC"
        note "added to $BASHRC"
    fi
    note "this shell is unchanged — run: export PATH=\"$SHIM_DIR:\$PATH\""
else
    say "PATH (skipped: --no-path)"
    note "to use the shim: export PATH=\"$SHIM_DIR:\$PATH\""
fi

say "Next"
note "open a new shell, then: type -a rust-analyzer   # the shim must come first"
note "then verify sharing:    scripts/lspmux-verify.py"

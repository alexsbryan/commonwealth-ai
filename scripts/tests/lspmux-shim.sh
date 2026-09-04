#!/usr/bin/env bash
# scripts/lspmux-shim/rust-analyzer — its two arms, pinned.
#
# The shim intercepts the name `rust-analyzer` on PATH so that LSP clients
# share one server (scripts/install-lspmux.sh).  Everything else that spawns
# `rust-analyzer` must come out the other side unchanged — above all the
# daemon's SCIP exporter, whose failure mode is not a broken build but a
# code-intel graph wiped to zero symbols.
#
# Hermetic: stub `rust-analyzer` and `lspmux` binaries in a mktemp PATH.  No
# rustup, no lspmux, no network, no server.  Runs in scripts/tests/run-all.sh
# (and therefore in pre-push) in well under a second.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

SHIM_SRC=scripts/lspmux-shim/rust-analyzer

fails=0
check() { # check <label> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '  ok   %s\n' "$1"
    else
        printf '  FAIL %s\n       expected: %s\n       actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}
check_contains() { # check_contains <label> <needle> <haystack>
    case "$3" in
        *"$2"*) printf '  ok   %s\n' "$1" ;;
        *) printf '  FAIL %s\n       expected to contain: %s\n       actual: %s\n' "$1" "$2" "$3"
           fails=$((fails + 1)) ;;
    esac
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/shim" "$tmp/real" "$tmp/mux" "$tmp/nohome"
cp "$SHIM_SRC" "$tmp/shim/rust-analyzer"
chmod +x "$tmp/shim/rust-analyzer"

# Stubs print their argv one item per line, so an assertion sees the EXACT
# vector — a shim that lost quoting or dropped an argument cannot pass by
# accident of string concatenation.
cat > "$tmp/real/rust-analyzer" <<'STUB'
#!/bin/sh
echo "REAL"
for a in "$@"; do echo "arg=$a"; done
STUB
cat > "$tmp/mux/lspmux" <<'STUB'
#!/bin/sh
echo "LSPMUX"
for a in "$@"; do echo "arg=$a"; done
STUB
chmod +x "$tmp/real/rust-analyzer" "$tmp/mux/lspmux"

REAL_ABS=$(cd "$tmp/real" && pwd -P)/rust-analyzer
FULL_PATH="$tmp/shim:$tmp/real:$tmp/mux:/usr/bin:/bin"

echo "lspmux shim — argument pass-through and multiplex arms"

# ── The SCIP export invocation, verbatim ────────────────────────────────
# corpus-engine-scip/src/scip_export.rs spawns exactly this shape. It is the
# failing input the whole shim is designed around.
out=$(PATH="$FULL_PATH" HOME="$tmp/nohome" "$tmp/shim/rust-analyzer" \
        scip . --config-path /tmp/cfg.json --output /tmp/out.scip 2>&1)
check "scip export reaches the real binary" \
    "$(printf 'REAL\narg=scip\narg=.\narg=--config-path\narg=/tmp/cfg.json\narg=--output\narg=/tmp/out.scip')" \
    "$out"

# ── Arguments containing spaces survive intact ──────────────────────────
out=$(PATH="$FULL_PATH" HOME="$tmp/nohome" "$tmp/shim/rust-analyzer" \
        scip . --output "/tmp/a dir/out.scip" 2>&1)
check "an argument with a space is not re-split" \
    "$(printf 'REAL\narg=scip\narg=.\narg=--output\narg=/tmp/a dir/out.scip')" \
    "$out"

# ── A single argument is still the pass-through arm ─────────────────────
out=$(PATH="$FULL_PATH" HOME="$tmp/nohome" "$tmp/shim/rust-analyzer" --version 2>&1)
check "--version reaches the real binary" "$(printf 'REAL\narg=--version')" "$out"

# ── No arguments = LSP stdio mode = the multiplexer ─────────────────────
out=$(PATH="$FULL_PATH" HOME="$tmp/nohome" "$tmp/shim/rust-analyzer" </dev/null 2>&1)
check "no arguments reaches lspmux client" \
    "$(printf 'LSPMUX\narg=client\narg=--server-path\narg=%s' "$REAL_ABS")" \
    "$out"

# ── The recursion guard: --server-path must be ABSOLUTE ─────────────────
# lspmux server does Command::new(server_path) with no PATH lookup, so an
# absolute path is what stops server -> shim -> client -> server. A relative
# or bare name here would loop until the box gave up.
server_path=$(printf '%s\n' "$out" | sed -n '4s/^arg=//p')
case "$server_path" in
    /*) printf '  ok   --server-path is absolute (%s)\n' "$server_path" ;;
    *)  printf '  FAIL --server-path is not absolute: %s\n' "$server_path"; fails=$((fails + 1)) ;;
esac
check "--server-path points at the real binary, not the shim" "$REAL_ABS" "$server_path"

# ── The shim never resolves to itself ───────────────────────────────────
# Same directory named three ways: absolutely, relatively, and through a
# symlink. `pwd -P` normalisation in the shim is what keeps all three from
# looking like a different directory that happens to hold a rust-analyzer.
ln -s "$tmp/shim" "$tmp/shim-link"
out=$( cd "$tmp" && PATH="$tmp/shim-link:./shim:$tmp/shim:$tmp/real:$tmp/mux:/usr/bin:/bin" \
        HOME="$tmp/nohome" "$tmp/shim/rust-analyzer" --version 2>&1 )
check "aliases of the shim directory are skipped, not exec'd" \
    "$(printf 'REAL\narg=--version')" "$out"

# ── lspmux missing: degrade, but SAY SO (ARCH §18.3) ────────────────────
out=$(PATH="$tmp/shim:$tmp/real:/usr/bin:/bin" HOME="$tmp/nohome" \
        "$tmp/shim/rust-analyzer" </dev/null 2>&1)
check_contains "without lspmux the shim still starts a server" "REAL" "$out"
check_contains "and names the substitution rather than hiding it" "PRIVATE" "$out"

# ── No rust-analyzer at all: refuse loudly ──────────────────────────────
out=$(PATH="$tmp/shim:$tmp/mux:/usr/bin:/bin" HOME="$tmp/nohome" \
        "$tmp/shim/rust-analyzer" --version 2>&1)
rc=$?
check "missing rust-analyzer exits 127" "127" "$rc"
check_contains "and says how to install it" "rustup component add rust-analyzer" "$out"

if [ "$fails" -eq 0 ]; then
    echo "lspmux shim: GREEN"
    exit 0
fi
echo "lspmux shim: $fails FAILED"
exit 1

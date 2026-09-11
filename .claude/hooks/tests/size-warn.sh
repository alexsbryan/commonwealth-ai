#!/bin/bash
# size-warn.py end to end: the REAL hook against a throwaway repo with a real
# baseline file. Each case is a shape the hook must produce or must not
# (ARCH §5: a gate nobody watched fail is not a gate):
#
#   - a non-Edit tool call is silent
#   - a pinned file at its ceiling warns, with the gate's own numbers
#   - the same file never warns twice in one session; a new session sees it
#   - a DIFFERENT pinned file still warns in the same session (per-file, not
#     per-session — the dedupe must not silence the whole session)
#   - a pinned file since cut well under its pin is silent (slack is the test,
#     not "is it pinned")
#   - an UNPINNED file at 1,180 lines warns as a NEW oversized file
#   - a small unpinned file, a new (nonexistent) file, a non-.rs file and a
#     vendored file are all silent
#   - SOVEREIGN_NO_SIZE_WARN=1 silences everything
#   - LINE_LIMIT/GROWTH_SLACK still match arch_gate.rs (parity with the gate
#     this hook speaks for — checked against the REAL source)
#
# Needs only git + python3 (no daemon, no sovereign binary, no cargo).
#   bash .claude/hooks/tests/size-warn.sh
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

HOOK="$PWD/.claude/hooks/size-warn.py"
GATE="$PWD/corpus-engine/xtask/src/arch_gate.rs"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
REPO="$ROOT/repo"
FAIL=0

ok() { printf '  ok   %s\n' "$1"; }
bad() { printf '  FAIL %s\n     %s\n' "$1" "$2"; FAIL=1; }

mk() { # mk <path> <lines>
  mkdir -p "$REPO/$(dirname "$1")"
  python3 -c "import sys;open(sys.argv[1],'w').write('// x\n'*int(sys.argv[2]))" "$REPO/$1" "$2"
}

run() { # run <session> <tool> <path> -> stdout (stderr dropped)
  printf '{"session_id":"%s","tool_name":"%s","cwd":"%s","tool_input":{"file_path":"%s"}}' \
    "$1" "$2" "$REPO" "$REPO/$3" | python3 "$HOOK" 2>/dev/null
}

run_err() { # same, but stderr merged — for the SOVEREIGN_SIZE_WARN_DEBUG case
  printf '{"session_id":"%s","tool_name":"%s","cwd":"%s","tool_input":{"file_path":"%s"}}' \
    "$1" "$2" "$REPO" "$REPO/$3" | python3 "$HOOK" 2>&1
}

# --- a repo shaped like this one ------------------------------------------
mkdir -p "$REPO" && git -C "$REPO" init -q
mkdir -p "$REPO/quality/baselines"
cat > "$REPO/quality/source-tree.toml" <<'EOF'
excluded_dirs = [ ".git", ".sovereign", "vendor", "node_modules" ]
EOF
cat > "$REPO/quality/baselines/oversized.txt" <<'EOF'
# arch-gate baseline — oversized .rs files (ARCH §3.1: > 1200 lines).
5820	src/frontdoor.rs
2736	src/state.rs
4000	src/shrunk.rs
EOF
mk src/frontdoor.rs 5820      # pinned, exactly at its pin -> 50 slack
mk src/state.rs 2780          # pinned, already over its ceiling (2736+50=2786)? no: 2780 -> 6 slack
mk src/shrunk.rs 3000         # pinned at 4000 but since cut -> 1050 slack, genuinely fine
mk src/small.rs 120           # unpinned, nowhere near
mk src/climbing.rs 1180       # unpinned, 20 from becoming NEW oversized
mk vendor/huge.rs 5000        # excluded by source-tree.toml
mk src/notes.md 9000          # not .rs (mk writes it anyway)

# --- cases ----------------------------------------------------------------
out=$(run s1 Read src/frontdoor.rs)
[ -z "$out" ] && ok "non-Edit tool is silent" || bad "non-Edit tool is silent" "$out"

out=$(run s1 Edit src/frontdoor.rs)
if echo "$out" | grep -q '5,820' && echo "$out" | grep -q '5,870' && echo "$out" | grep -q '50 lines of room'; then
  ok "pinned file at its ceiling warns with the gate's numbers"
else bad "pinned file at its ceiling warns" "$out"; fi

out=$(run s1 Edit src/frontdoor.rs)
[ -z "$out" ] && ok "same file is silent the second time in one session" || bad "second time silent" "$out"

out=$(run s1 Write src/state.rs)
echo "$out" | grep -q '6 lines of room' && ok "a different pinned file still warns in the same session" \
  || bad "different pinned file in same session" "$out"

out=$(run s2 Edit src/frontdoor.rs)
echo "$out" | grep -q '5,820' && ok "a NEW session sees the file again" || bad "new session sees it" "$out"

out=$(run s3 Edit src/shrunk.rs)
[ -z "$out" ] && ok "a pinned file since cut under its pin is silent" || bad "shrunk file silent" "$out"

out=$(run s3 Edit src/climbing.rs)
if echo "$out" | grep -q 'NOT yet baselined' && echo "$out" | grep -q '20 lines of room'; then
  ok "unpinned file near 1200 warns as a NEW oversized file"
else bad "unpinned file near 1200 warns" "$out"; fi

out=$(run s3 Edit src/small.rs)
[ -z "$out" ] && ok "a small unpinned file is silent" || bad "small file silent" "$out"

out=$(run s3 Write src/brand_new.rs)
[ -z "$out" ] && ok "a new (nonexistent) file is silent — the outcome we want" || bad "new file silent" "$out"

out=$(run s3 Edit src/notes.md)
[ -z "$out" ] && ok "a non-.rs file is silent" || bad "non-.rs silent" "$out"

out=$(run s3 Edit vendor/huge.rs)
[ -z "$out" ] && ok "a vendored file is silent (source-tree.toml is the decider)" || bad "vendor silent" "$out"

out=$(SOVEREIGN_NO_SIZE_WARN=1 run s4 Edit src/frontdoor.rs)
[ -z "$out" ] && ok "SOVEREIGN_NO_SIZE_WARN=1 silences everything" || bad "opt-out" "$out"

out=$(SOVEREIGN_SIZE_WARN_DEBUG=1 run_err s5 Edit src/small.rs)
echo "$out" | grep -q 'slack 1080 -> silent' \
  && ok "DEBUG=1 makes a SILENT verdict legible (ARCH §9.1)" || bad "debug trace on silence" "$out"

# --- parity with the gate this hook speaks for ----------------------------
g_limit=$(grep -oE 'LINE_LIMIT: usize = [0-9]+' "$GATE" | grep -oE '[0-9]+')
g_slack=$(grep -oE 'GROWTH_SLACK: usize = [0-9]+' "$GATE" | grep -oE '[0-9]+')
h_limit=$(grep -oE '^LINE_LIMIT = [0-9]+' "$HOOK" | grep -oE '[0-9]+')
h_slack=$(grep -oE '^GROWTH_SLACK = [0-9]+' "$HOOK" | grep -oE '[0-9]+')
if [ "$g_limit" = "$h_limit" ] && [ "$g_slack" = "$h_slack" ]; then
  ok "constants match arch_gate.rs (limit $g_limit, slack $g_slack)"
else bad "constants match arch_gate.rs" "gate=$g_limit/$g_slack hook=$h_limit/$h_slack"; fi

[ "$FAIL" = 0 ] && echo "size-warn: all cases pass" || echo "size-warn: FAILURES above"
exit "$FAIL"

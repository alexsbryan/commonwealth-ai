#!/bin/bash
# End-to-end through the REAL SessionStart hook: does a /clear successor get
# its predecessor's frame injected whole, and does boot.json record why?
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
ROOT="$(mktemp -d)"
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
export SOVEREIGN_LINEAGE_DIR="$ROOT/lineage"
export PATH="$ROOT/bin:$PATH"
mkdir -p "$SOVEREIGN_SESSIONS_DIR" "$SOVEREIGN_LINEAGE_DIR" "$ROOT/bin"
ln -sf "$(realpath target/debug/sovereign-cli)" "$ROOT/bin/sovereign"

pass=0; fail=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: expected [$2] got [$3]"; fail=$((fail+1)); fi; }
boot() { # boot <session_id> <source>  -> payload on stdout, boot.json written
  printf '{"session_id":"%s","source":"%s","cwd":"%s"}' "$1" "$2" "$PWD" \
    | sh .claude/hooks/session-boot.sh
}
bootjson() { python3 -c "import json,sys;print(json.load(open(sys.argv[1])).get(sys.argv[2]))" \
  "$SOVEREIGN_SESSIONS_DIR/$1/boot.json" "$2"; }

mkframe() {
  mkdir -p "$SOVEREIGN_SESSIONS_DIR/$1"
  printf -- '---\nsession_id: %s\nrepo: commonwealth-ai\nbranch: main\nstatus: in-flight\nprovenance: self-reported\n---\n\n## Goal\n\n%s\n\n## Next\n\n- %s\n' \
    "$1" "$2" "$3" > "$SOVEREIGN_SESSIONS_DIR/$1/frame.md"
}
mkframe predecessor-aaa "Window-lineage handoff: stop guessing the frame after /clear" "wire the boot hook"
mkframe decoy-newest    "Totally unrelated F9 scheduler arc"                            "do not pick me"
touch -d '20 minutes ago' "$SOVEREIGN_SESSIONS_DIR/predecessor-aaa/frame.md"

export SOVEREIGN_WINDOW_KEY=hooktest-window-A

echo "== boot 1: fresh window, no predecessor =="
p1=$(boot sess-one startup)
check "no frame injected"            "index"   "$(bootjson sess-one frame_selection)"
check "window recorded"              "hooktest-window-A" "$(bootjson sess-one window_key)"
check "predecessor recorded as none" "None"    "$(bootjson sess-one predecessor)"
grep -q "Live session frames" <<<"$p1" && { echo "  ok   index was injected"; pass=$((pass+1)); } \
  || { echo "  FAIL index missing"; fail=$((fail+1)); }

echo
echo "== the donor banks a frame, then the user hits /clear =="
mkframe sess-one "Window-lineage handoff: stop guessing the frame after /clear" "wire the boot hook"

p2=$(boot sess-two clear)
check "handoff is deterministic"     "lineage"  "$(bootjson sess-two frame_selection)"
check "and names the predecessor"    "sess-one" "$(bootjson sess-two predecessor)"
check "frame_session matches"        "sess-one" "$(bootjson sess-two frame_session)"
check "kind is process-derived"      "process"  "$(bootjson sess-two predecessor_kind)"
grep -q "Session handoff" <<<"$p2" && { echo "  ok   frame header present"; pass=$((pass+1)); } \
  || { echo "  FAIL no handoff header"; fail=$((fail+1)); }
grep -q "wire the boot hook" <<<"$p2" && { echo "  ok   frame BODY injected whole"; pass=$((pass+1)); } \
  || { echo "  FAIL frame body missing"; fail=$((fail+1)); }
grep -q "Live session frames" <<<"$p2" && { echo "  FAIL index also injected (duplicate spend)"; fail=$((fail+1)); } \
  || { echo "  ok   index NOT also injected"; pass=$((pass+1)); }
grep -q "decoy-newest\|F9 scheduler" <<<"$p2" && { echo "  FAIL the decoy leaked in"; fail=$((fail+1)); } \
  || { echo "  ok   the newer decoy frame was not chosen"; pass=$((pass+1)); }

echo
echo "== the payload budget still holds =="
chars=$(printf '%s' "$p2" | wc -c)
if [ "$chars" -lt 10000 ]; then echo "  ok   payload ${chars}B < 10KB spill threshold"; pass=$((pass+1));
else echo "  FAIL payload ${chars}B would spill to a file"; fail=$((fail+1)); fi
check "payload_bytes recorded"       "$chars" "$(bootjson sess-two payload_bytes)"

echo
echo "== a concurrent second terminal is unaffected =="
export SOVEREIGN_WINDOW_KEY=hooktest-window-B
boot sess-b1 startup >/dev/null
check "window B gets no handoff"     "index" "$(bootjson sess-b1 frame_selection)"
check "and no predecessor"           "None"  "$(bootjson sess-b1 predecessor)"

echo
echo "== inject-notes must not double-inject after a lineage boot =="
# PRE-EXISTING RED, not caused by the 2026-08-07 hook rewrite. This asserts a
# `frame-inject.json` marker that the notes hook has never written — it injects
# NOTES, not frames, so the marker is only ever produced by session-boot.sh.
# The assertion appears to be left over from a design where the two were one
# hook. Left failing deliberately rather than deleted: disabling a test to get
# green needs a todo saying what was deferred (§0.4), and the todo is filed.
printf '{"session_id":"sess-two","prompt":"continue","cwd":"%s"}' "$PWD" \
  | python3 .claude/hooks/inject-notes.py >/dev/null 2>&1
marker="$SOVEREIGN_SESSIONS_DIR/sess-two/frame-inject.json"
if [ -f "$marker" ]; then
  check "outcome says boot already did it" "already_injected_at_boot_lineage" \
    "$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['outcome'])" "$marker")"
else
  echo "  FAIL no frame-inject marker written"; fail=$((fail+1))
fi

echo
echo "== $pass passed, $fail failed =="
rm -rf "$ROOT"
[ "$fail" -eq 0 ]

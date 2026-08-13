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
# `touch -d '20 minutes ago'` is GNU-only; BSD touch answers "out of range or
# illegal time specification" and leaves the file at NOW, which silently
# un-ages every fixture on macOS. One helper, both platforms.
age_file() { # age_file <path> <minutes>
  local ts
  ts="$(date -v-"$2"M +%Y%m%d%H%M 2>/dev/null || date -d "$2 minutes ago" +%Y%m%d%H%M)"
  touch -t "$ts" "$1"
}

mkframe predecessor-aaa "Window-lineage handoff: stop guessing the frame after /clear" "wire the boot hook"
mkframe decoy-newest    "Totally unrelated F9 scheduler arc"                            "do not pick me"
age_file "$SOVEREIGN_SESSIONS_DIR/predecessor-aaa/frame.md" 20

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
chars=$(printf '%s' "$p2" | wc -c | tr -d ' ')   # BSD wc pads; the check is exact-match
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
echo "== a STALE binding beside a live frame names both =="
# The 2026-08-13 defect: the hook injected a 16h lineage frame as *the*
# predecessor while an 11-minute in-flight frame for the same repo existed,
# and the successor spent 120k+ tokens re-deriving it. The handoff stays the
# lineage frame — it is an observation — but the fresher one must be named.
export SOVEREIGN_WINDOW_KEY=hooktest-window-C
boot sess-stale startup >/dev/null          # window C binds to sess-stale
mkframe sess-stale    "The workstream this terminal ran yesterday" "old next item"
mkframe freshest-live "Where the work actually is right now"       "the live next item"
age_file "$SOVEREIGN_SESSIONS_DIR/sess-stale/frame.md" 960   # 16h
p3=$(boot sess-successor clear)
check "the lineage frame is still the handoff" "lineage"    "$(bootjson sess-successor frame_selection)"
check "and it is still the STALE one"          "sess-stale" "$(bootjson sess-successor frame_session)"
check "the fresher frame was named"            "True"       "$(bootjson sess-successor fresher_frame_named)"
grep -q "fresher IN-FLIGHT frame exists" <<<"$p3" && { echo "  ok   advisory reached the payload"; pass=$((pass+1)); } \
  || { echo "  FAIL no advisory in the payload"; fail=$((fail+1)); }
grep -q "sovereign session frames freshest" <<<"$p3" && { echo "  ok   ...naming the verb that reads it"; pass=$((pass+1)); } \
  || { echo "  FAIL advisory does not name the deref verb"; fail=$((fail+1)); }
# The other direction, on the healthy handoff from earlier in this file: a
# fresh predecessor must produce NO advisory, or the signal becomes noise.
grep -q "fresher IN-FLIGHT frame exists" <<<"$p2" && { echo "  FAIL advisory fired on a healthy handoff"; fail=$((fail+1)); } \
  || { echo "  ok   silent when the binding is fresh"; pass=$((pass+1)); }

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

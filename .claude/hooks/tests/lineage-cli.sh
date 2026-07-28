#!/bin/bash
# End-to-end proof for window-lineage frame handoff.
# Exercises the REAL CLI against an isolated store — no mocks, no unit-test
# shortcuts. Reproduces the measured failure and shows it fixed.
set -u
BIN="$(realpath "${BIN:-target/debug/sovereign-cli}")"
ROOT="$(mktemp -d)"
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
export SOVEREIGN_LINEAGE_DIR="$ROOT/lineage"
mkdir -p "$SOVEREIGN_SESSIONS_DIR" "$SOVEREIGN_LINEAGE_DIR"

pass=0; fail=0
check() { # check <label> <expected> <actual>
  if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
  else echo "  FAIL $1: expected [$2] got [$3]"; fail=$((fail+1)); fi
}
frame() { # frame <session> <branch> <goal>
  mkdir -p "$SOVEREIGN_SESSIONS_DIR/$1"
  printf -- '---\nsession_id: %s\nrepo: commonwealth-ai\nbranch: %s\nstatus: in-flight\nprovenance: self-reported\n---\n\n## Goal\n\n%s\n\n## Next\n\n- keep going\n' \
    "$1" "$2" "$3" > "$SOVEREIGN_SESSIONS_DIR/$1/frame.md"
}
q() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval('d'+sys.argv[1]) if True else '')" "$1"; }
frames() { "$BIN" session frames --json --repo commonwealth-ai --branch main "$@" 2>/dev/null; }

echo "== the measured failure: two terminals, four workstreams =="
# Three unrelated frames + the one that actually belongs to window A.
frame w-pred      main "Post-crash resilience: local-fit gate and CPU-only exemption"
frame f9-unrelated main "F9 scheduler quality: arm 0 was never the shipped system"
frame other-1     main "Cloud tensor peer over iroh"
frame other-2     main "Remote-support surface for the 20-person mesh"
# Make the unrelated frames NEWER than the real predecessor — the exact
# condition under which recency picked wrong.
touch -d '10 minutes ago' "$SOVEREIGN_SESSIONS_DIR/w-pred/frame.md"
touch -d '1 minute ago'   "$SOVEREIGN_SESSIONS_DIR/f9-unrelated/frame.md"

echo
echo "-- Window A boots fresh (no lineage yet), then /clears --"
export SOVEREIGN_WINDOW_KEY=windowA
out=$(frames --claim-window w-pred)
check "first boot in a window has no predecessor" "None" "$(echo "$out" | q "['predecessor']")"
check "window is resolved from the declared key"  "windowA" "$(echo "$out" | q "['window']['key']")"

# /clear: new session id, same terminal.
out=$(frames --claim-window w-succ --for-prompt "continue with everything from the frame")
check "predecessor is the previous occupant"     "w-pred" "$(echo "$out" | q "['predecessor']['session_id']")"
check "predecessor is process-derived"           "process" "$(echo "$out" | q "['predecessor']['kind']")"
check "predecessor's frame is on disk"           "True"    "$(echo "$out" | q "['predecessor']['has_frame']")"
check "and it also leads the ranked index"       "w-pred"  "$(echo "$out" | q "['candidates'][0]['session_id']")"
check "flagged same_window in the signals"       "True"    "$(echo "$out" | q "['candidates'][0]['signals']['same_window']")"

echo
echo "-- REGRESSION: the old ranker picked the newest/overlapping frame --"
out=$(frames --no-lineage --for-prompt "continue with everything from the frame")
old_pick=$(echo "$out" | q "['candidates'][0]['session_id']")
if [ "$old_pick" = "w-pred" ]; then
  echo "  note the prompt did not mislead the fallback ranker this time (picked $old_pick)"
else
  echo "  ok   without lineage the ranker still picks the wrong frame ($old_pick) — that is the bug"
  pass=$((pass+1))
fi

echo
echo "-- Window B, concurrent, must NOT inherit window A's predecessor --"
export SOVEREIGN_WINDOW_KEY=windowB
out=$(frames --claim-window b-first)
check "a second terminal starts with no lineage" "None" "$(echo "$out" | q "['predecessor']")"
out=$(frames --claim-window b-second)
check "and inherits only its own history"        "b-first" "$(echo "$out" | q "['predecessor']['session_id']")"

echo
echo "-- Explicit attach: continue a workstream in a window that never ran it --"
"$BIN" session attach f9-unrelated >/dev/null 2>&1
out=$(frames --claim-window b-third)
check "attach overrides process lineage"         "f9-unrelated" "$(echo "$out" | q "['predecessor']['session_id']")"
check "and is reported as explicit, not inferred" "explicit"    "$(echo "$out" | q "['predecessor']['kind']")"
# Claiming rebinds to process kind, so an attach is consumed once, not sticky.
out=$(frames --claim-window b-fourth)
check "attach is consumed by the next boot"      "b-third" "$(echo "$out" | q "['predecessor']['session_id']")"
check "which reverts to process lineage"         "process" "$(echo "$out" | q "['predecessor']['kind']")"

echo
echo "-- Predecessor that banked no frame is reported honestly --"
export SOVEREIGN_WINDOW_KEY=windowC
frames --claim-window ghost >/dev/null
out=$(frames --claim-window after-ghost)
check "lineage still resolves"                   "ghost" "$(echo "$out" | q "['predecessor']['session_id']")"
check "but has_frame is false, not a fake handoff" "False" "$(echo "$out" | q "['predecessor']['has_frame']")"
check "path is null"                             "None"  "$(echo "$out" | q "['predecessor']['path']")"

echo
echo "-- Degradation: no window at all => old behaviour, no crash --"
unset SOVEREIGN_WINDOW_KEY
export SOVEREIGN_HARNESS_COMM=definitely-not-a-real-process
out=$(frames)
check "window is null"                           "None" "$(echo "$out" | q "['window']")"
check "predecessor is null"                      "None" "$(echo "$out" | q "['predecessor']")"
check "index still returned"                     "True" "$(python3 -c "import json,sys;print(len(json.loads(sys.stdin.read())['candidates'])>0)" <<<"$out")"
unset SOVEREIGN_HARNESS_COMM

echo
echo "-- detach --"
export SOVEREIGN_WINDOW_KEY=windowB
"$BIN" session attach --clear >/dev/null
out=$(frames)
check "detached window has no predecessor"       "None" "$(echo "$out" | q "['predecessor']")"

echo
echo "== $pass passed, $fail failed =="
rm -rf "$ROOT"
[ "$fail" -eq 0 ]

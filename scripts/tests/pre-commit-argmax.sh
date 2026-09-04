#!/usr/bin/env bash
# Negative controls for scripts/pre-commit.sh — the work-atlas collision hook.
#
# WHY THIS EXISTS. The hook is ADVISORY: every path exits 0 by operator
# directive (2026-08-06), so it has no exit code anyone reads and its only
# output is text. That makes it the perfect place for a check to die without
# anybody noticing, and it did — twice, each time as the fix for the last one:
#
#   1. `sovereign … | python3 - <<PY` piped the atlas into a process whose
#      stdin was already the heredoc. The payload was discarded and the hook
#      could never fire.
#   2. The fix, `export ATLAS="$(sovereign …)"`, moved the payload into the
#      ENVIRONMENT. `work_in_flight --scope=` is the "everything" form; on this
#      host the atlas reached 4,716,818 bytes against an ARG_MAX of 1,048,576,
#      so execve(2) refused every child the script spawned — python3, grep,
#      rustfmt — with E2BIG. All of it exited 0. Every commit made on this host
#      while the atlas was that size had both checks silently off.
#
# Neither failure is visible in a green run, and no cargo test can reach a
# shell script's exec boundary (ARCH §18.1: a gate you have not watched fail is
# not a gate). So this suite drives the REAL hook with a stubbed `sovereign` on
# PATH and a deliberately colliding peer claim, and asserts the warning appears
# WITH A 5 MB ATLAS IN PLAY — the exact condition that killed it.
#
# Case 0 is a negative control on the harness itself: it reconstructs the old
# export-the-payload form and asserts it DOES die. Without it, cases 1-4 could
# pass on a machine whose ARG_MAX made the bug unreproducible, and the suite
# would be decoration.
#
# No cargo, no daemon, no network; nothing written outside the temp dir.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
HOOK="$ROOT/scripts/pre-commit.sh"
[[ -f "$HOOK" ]] || { echo "cannot find $HOOK"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin"

# How much padding makes the environment path fail. Sized from the measured
# incident (4.7 MB) rather than from ARG_MAX, so the test keeps reproducing the
# real shape on a host with a larger limit.
PAD_BYTES=4700000

# ── The `sovereign` stub ───────────────────────────────────────────────────
# Answers the two calls the hook makes. The atlas carries one PEER claim on
# alpha.rs (node_is_self false) plus PAD_BYTES of filler in an ignored key, so
# every case below runs against a payload the size of the one that broke it.
cat > "$T/bin/sovereign" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  "mesh status")
      echo "37f17554b6c4ff29aaaa  peer-node"
      echo "b88252e4325bc377bbbb  this-node  *"
      ;;
  *work_in_flight*)
      python3 -c '
import json, os, sys
pad = "x" * int(os.environ["PAD_BYTES"])
doc = {
  "scope": "", "match_mode": "file",
  "claims": [{
      "node_id": os.environ["CLAIM_NODE"],
      "node_is_self": os.environ["CLAIM_SELF"] == "1",
      "scopes": ["alpha.rs"],
      "intent": "rewriting the alpha module",
  }],
  "observations": [],
  "_pad": pad,
}
sys.stdout.write(json.dumps(doc))
'
      ;;
  *) exit 0 ;;
esac
STUB
chmod +x "$T/bin/sovereign"

export PATH="$T/bin:$PATH"
export PAD_BYTES
export CO_PRECOMMIT_STAGED="alpha.rs"

rc=0
pass() { echo "  ok    $1"; }
flunk() { echo "  FAIL  $1 — $2"; rc=1; }

echo "pre-commit-argmax:"

# ── Case 0: the negative control. ──────────────────────────────────────────
# The old form, reconstructed. If this does NOT die, the host cannot reproduce
# the bug and cases 1-2 prove nothing about it — so say so loudly rather than
# reporting a green nobody earned (§18.3).
cat > "$T/old-form.sh" <<'OLD'
#!/usr/bin/env bash
set -uo pipefail
ATLAS="$(sovereign tools call work_in_flight --scope= --match_mode=file --format json 2>/dev/null)"
export ATLAS
python3 -c 'print("CHILD RAN")'
OLD
CLAIM_NODE="node-37f17554b6c4ff29" CLAIM_SELF=0 \
    bash "$T/old-form.sh" >"$T/old.out" 2>&1
if grep -q "CHILD RAN" "$T/old.out"; then
    flunk "negative control: the old export-the-payload form still dies" \
          "its child ran, so this host cannot reproduce E2BIG at ${PAD_BYTES}B and cases 1-2 are not evidence"
else
    pass "negative control: the old export-the-payload form dies (child never ran)"
fi

# ── Case 1: THE POINT. A peer claim is reported with a 4.7 MB atlas. ───────
CLAIM_NODE="node-37f17554b6c4ff29" CLAIM_SELF=0 \
    bash "$HOOK" >"$T/warn.out" 2>&1
if grep -q "work-atlas WARNING" "$T/warn.out" && grep -q "alpha.rs" "$T/warn.out"; then
    pass "a peer claim is reported at a ${PAD_BYTES}-byte atlas"
else
    flunk "a peer claim is reported at a ${PAD_BYTES}-byte atlas" "no warning"
    sed 's/^/          /' "$T/warn.out" | head -5
fi

# ── Case 2: and it is not just "always warns" — self is still filtered. ────
CLAIM_NODE="node-b88252e4325bc377" CLAIM_SELF=1 \
    bash "$HOOK" >"$T/self.out" 2>&1
if grep -q "work-atlas WARNING" "$T/self.out"; then
    flunk "your own claim is not a collision" "warned on node_is_self=true"
else
    pass "your own claim is not a collision"
fi

# ── Case 3: no atlas at all stays silent (the daemon-down path). ───────────
cat > "$T/bin/sovereign" <<'DOWN'
#!/usr/bin/env bash
case "$*" in
  "mesh status") echo "b88252e4325bc377bbbb  this-node  *" ;;
  *) exit 1 ;;
esac
DOWN
chmod +x "$T/bin/sovereign"
bash "$HOOK" >"$T/down.out" 2>&1
if grep -qE "work-atlas (WARNING|COULD-NOT-RUN)" "$T/down.out"; then
    flunk "a down daemon stays silent" "it spoke: $(head -1 "$T/down.out")"
else
    pass "a down daemon stays silent"
fi

# ── Case 4: a child that cannot run is REPORTED, not counted as clean. ─────
# The third verdict. Shadow python3 with a stub that exits 126 — exactly what
# execve refusing the binary looked like — and require the hook to say so.
cat > "$T/bin/python3" <<'NOPY'
#!/usr/bin/env bash
exit 126
NOPY
chmod +x "$T/bin/python3"
bash "$HOOK" >"$T/norun.out" 2>&1
hook_rc=$?
if grep -q "work-atlas COULD-NOT-RUN" "$T/norun.out"; then
    pass "a child that cannot run is reported as could-not-run"
else
    flunk "a child that cannot run is reported as could-not-run" \
          "silent — this is the failure mode the suite exists for"
fi
if [[ "$hook_rc" -ne 0 ]]; then
    flunk "the hook stays advisory on every path" "exited $hook_rc"
else
    pass "the hook stays advisory on every path"
fi

exit "$rc"

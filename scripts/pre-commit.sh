#!/usr/bin/env bash
# pre-commit.sh — warn-only work-atlas collision check.
#
# Comaintainer M0 supervised directive, operator-approved 2026-08-06:
# before a commit lands, surface any OTHER mesh node with a live claim
# or a fresh edit observation on the files being committed. Advisory by
# design — every path exits 0; the human decides. Blocking here would
# make the atlas a gate nobody asked for (grades are soft signals, and
# a peer's stale TTL must never hold up a commit).
#
# Self-noise filter: entries from this node are skipped — your own
# session's claim on the files you are committing is not a collision.
# Runnable by hand: ./scripts/pre-commit.sh
set -uo pipefail

command -v sovereign >/dev/null 2>&1 || exit 0
# CO_PRECOMMIT_* overrides exist so the warn path can be exercised by
# hand without staging a peer's file (§18.1: watch it fire).
STAGED="${CO_PRECOMMIT_STAGED:-$(git diff --cached --name-only 2>/dev/null)}"
[ -n "$STAGED" ] || exit 0
# Self = the starred row in mesh status; work-atlas ids are node-<hex16>.
SELF="${CO_PRECOMMIT_SELF:-$(sovereign mesh status 2>/dev/null \
    | awk '$NF=="*"{print substr($1,1,16)}')}"
# Via a TEMP FILE, and neither via a pipe nor via the environment.
#
# Both of the obvious ways to hand a payload to the heredoc below are broken,
# and each broke this hook silently — which is the whole reason this comment is
# four times the length of the code it guards.
#
#   `… | python3 - <<PY` hands stdin to the HEREDOC and discards the pipe. A
#   hook that can never fire (watched failing exactly that way, §18.1).
#
#   `export ATLAS="$(…)"` fixed that and built a worse one a layer out. The
#   environment block is copied into every child process, and `execve(2)`
#   rejects it past ARG_MAX. `work_in_flight --scope=` is the documented
#   "everything" form: measured on this host 2026-09-04 the atlas was
#   4,716,818 bytes against an ARG_MAX of 1,048,576, so EVERY child — python3,
#   grep, rustfmt, even getconf — died `Argument list too long` before running
#   a line. The hook is advisory by the 2026-08-06 directive, so all of it
#   exited 0 and the commit sailed through. Both checks below had been dead on
#   this host for as long as the atlas had been that size.
#
# A path is bounded. The payload is read by the one process that needs it, and
# no size of atlas can push a child over the exec limit again. `mktemp -d`
# rather than one file so the staged list rides the same bound: it is small
# today and is a list of file paths, i.e. exactly the kind of thing that grows
# without anyone deciding it should.
HOOK_TMP="$(mktemp -d -t precommit-atlas.XXXXXX)" || exit 0
trap 'rm -rf "$HOOK_TMP"' EXIT
ATLAS_FILE="$HOOK_TMP/atlas.json"
STAGED_FILE="$HOOK_TMP/staged.txt"
sovereign tools call work_in_flight --scope= --match_mode=file \
    --format json >"$ATLAS_FILE" 2>/dev/null
printf '%s\n' "$STAGED" >"$STAGED_FILE"
export SELF ATLAS_FILE STAGED_FILE

python3 - <<'PY'
import json, os, sys
try:
    with open(os.environ["ATLAS_FILE"], encoding="utf-8") as fh:
        atlas = json.loads(fh.read() or "{}")
except Exception:
    sys.exit(0)  # daemon down or malformed — advisory hook stays silent
with open(os.environ["STAGED_FILE"], encoding="utf-8") as fh:
    staged = set(fh.read().split())
self_hex = os.environ.get("SELF", "")
def node_str(node): return (node or "")  # null node_id = legacy pre-fix-1 claim
def is_self(entry):
    # Trust the atlas's OWN answer first. The daemon computes `node_is_self`
    # against the identity the observer actually stamped rows with; scraping
    # `mesh status` for the starred row re-derives the same fact from a second
    # source, and the two can disagree — observed 2026-08-20, where the star
    # said 37f17554b6c4ff29 while every locally-observed row was stamped
    # b88252e4325bc377 (node_is_self=true). On that disagreement the scrape
    # reports EVERY one of your own edits as a peer's, and a warning that always
    # fires is one people learn to click past — which costs the real collision
    # it exists to catch (§10.6: one decider; §18.1: a guard nobody trusts).
    v = entry.get("node_is_self")
    if isinstance(v, bool):
        return v
    return bool(self_hex) and node_str(entry.get("node_id")).removeprefix("node-") == self_hex
def touches(scope):
    scope = scope.rstrip("/")
    return any(f == scope or f.startswith(scope + "/") or scope.startswith(f)
               for f in staged)
warns = []
for c in atlas.get("claims", []):
    if not is_self(c):
        for s in c.get("scopes", []):
            if touches(s):
                warns.append(f"  claim   {s}  ({c.get('node_id','?')}) — "
                             f"{c.get('intent','')[:90]}")
                break
for o in atlas.get("observations", []):
    if not is_self(o) and o.get("file_path", "") in staged:
        warns.append(f"  {o.get('confidence','?'):7} {o['file_path']}  "
                     f"({o.get('node_id','?')}, {o.get('event_count',0)} edits)")
if warns:
    print("work-atlas WARNING (advisory, commit proceeds): peers are on "
          "files in this commit —")
    print("\n".join(warns))
    print("  detail: work_in_flight(scope, match_mode=\"file\") · this "
          "never blocks (warn-only by operator directive 2026-08-06)")
PY
atlas_rc=$?

# FOUR VERDICTS, NOT TWO (§18.2). Everything above exits 0 on every path it
# understands, INCLUDING "the daemon is down" — so any non-zero here means the
# check did not run at all, and that is a third verdict, not a pass. It is what
# the ARG_MAX bug looked like from outside for weeks: exit 126, a line of noise
# on stderr, and a commit that recorded fine. Advisory still (the directive
# holds; nothing here blocks), but never again silent.
if [ "$atlas_rc" -ne 0 ]; then
    printf 'work-atlas COULD-NOT-RUN (advisory, commit proceeds): the collision\n'
    printf '  check exited %s without rendering a verdict — this is NOT "no peers".\n' "$atlas_rc"
    printf '  126/127 means the child could not be exec-ed at all (ARG_MAX, or no\n'
    printf '  python3); anything else is a crash. Reproduce: ./scripts/pre-commit.sh\n'
fi

# ── Formatting notice (separable from the atlas check above) ───────────────
#
# WHY HERE. The pre-push rustfmt gate is already whole-workspace
# (`cargo fmt --all --check`), so nothing unformatted can reach origin — that
# part works. What does NOT work is WHO pays: many commits land on local main
# between pushes, so the gate fires for whoever pushes next, on someone else's
# code. Cutting cli-v0.5.0 on 2026-08-08 absorbed three separate rustfmt
# sweeps across 34, 1 and 1 files, none of them the release's own changes.
#
# This moves the signal to the author, at the moment the code is written, for
# the cost of running rustfmt over the STAGED .rs files only (milliseconds —
# no cargo, no workspace scan).
#
# WARN-ONLY, deliberately: the operator directive of 2026-08-06 makes this
# hook advisory, and that is the right call here too — a formatting nit must
# never stand between someone and recording their work. The push gate remains
# the thing that actually blocks.
STAGED_RS="$(printf '%s\n' "$STAGED" | grep -E '\.rs$' || true)"
if [ -n "$STAGED_RS" ] && command -v rustfmt >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    UNFMT_PATHS="$(printf '%s\n' $STAGED_RS | while read -r f; do
        [ -f "$f" ] || continue
        rustfmt --edition 2021 --check "$f" >/dev/null 2>&1 || printf '%s ' "$f"
    done)"
    if [ -n "$UNFMT_PATHS" ]; then
        printf 'rustfmt NOTICE (advisory, commit proceeds): staged file(s) need formatting —\n'
        for f in $UNFMT_PATHS; do printf '    %s\n' "$f"; done
        # Name the SCOPED command, not `cargo fmt --all`. This repo is routinely
        # a shared checkout with several agents mid-edit; a workspace-wide fmt
        # reformats their in-flight files, which is the exact "someone else's
        # problem" this notice exists to prevent. The unformatted set is already
        # known here, so hand it back rather than a blunt instrument.
        printf '  fix (your staged files only):\n    rustfmt --edition 2021 %s\n' "$UNFMT_PATHS"
        printf '  the pre-push gate is whole-workspace and WILL block on this, so do not\n'
        printf '  leave it. Avoid `cargo fmt --all` unless you own every uncommitted\n'
        printf '  change in the tree — it rewrites other agents'"'"' files too.\n'
    fi
fi
exit 0

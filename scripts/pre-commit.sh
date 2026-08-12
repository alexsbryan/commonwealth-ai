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
# Via env, NOT a pipe: `… | python3 - <<heredoc` hands stdin to the
# heredoc and silently discards the pipe — a hook that can never fire
# (watched failing exactly that way before this line existed, §18.1).
ATLAS="$(sovereign tools call work_in_flight --scope= --match_mode=file \
    --format json 2>/dev/null)"
export STAGED SELF ATLAS

python3 - <<'PY'
import json, os, sys
try:
    atlas = json.loads(os.environ.get("ATLAS") or "{}")
except Exception:
    sys.exit(0)  # daemon down or malformed — advisory hook stays silent
staged = set(os.environ["STAGED"].split())
self_hex = os.environ.get("SELF", "")
def node_str(node): return (node or "")  # null node_id = legacy pre-fix-1 claim
def is_self(node): return bool(self_hex) and node_str(node).removeprefix("node-") == self_hex
def touches(scope):
    scope = scope.rstrip("/")
    return any(f == scope or f.startswith(scope + "/") or scope.startswith(f)
               for f in staged)
warns = []
for c in atlas.get("claims", []):
    if not is_self(c.get("node_id")):
        for s in c.get("scopes", []):
            if touches(s):
                warns.append(f"  claim   {s}  ({c.get('node_id','?')}) — "
                             f"{c.get('intent','')[:90]}")
                break
for o in atlas.get("observations", []):
    if not is_self(o.get("node_id")) and o.get("file_path", "") in staged:
        warns.append(f"  {o.get('confidence','?'):7} {o['file_path']}  "
                     f"({o.get('node_id','?')}, {o.get('event_count',0)} edits)")
if warns:
    print("work-atlas WARNING (advisory, commit proceeds): peers are on "
          "files in this commit —")
    print("\n".join(warns))
    print("  detail: work_in_flight(scope, match_mode=\"file\") · this "
          "never blocks (warn-only by operator directive 2026-08-06)")
PY

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
    UNFMT="$(printf '%s\n' $STAGED_RS | while read -r f; do
        [ -f "$f" ] || continue
        rustfmt --edition 2021 --check "$f" >/dev/null 2>&1 || printf '    %s\n' "$f"
    done)"
    if [ -n "$UNFMT" ]; then
        printf 'rustfmt NOTICE (advisory, commit proceeds): staged file(s) need formatting —\n%s\n' "$UNFMT"
        printf '  fix: cargo fmt --all   · the pre-push gate WILL block on this, and it is\n'
        printf '       whole-workspace, so leaving it makes it someone else'"'"'s problem to fix.\n'
    fi
fi
exit 0

#!/usr/bin/env bash
# extension-decision.sh — the revision-bound design record an order carries.
#
# WHY THIS EXISTS. The design-reuse lane established that a model can name a
# home and still build a parallel one (or claim growth while citing nothing).
# The order is where that decision is made and handed to a worker, so the
# order is where it must be checked. These controls pin the boundary:
#
#   - an ABSENT / all-(none) Extension is legal (small edits) — a NUDGE;
#   - a PRESENT Extension is a record: missing labels BLOCK;
#   - claiming growth (new owner/store/state/effect/dependency) WITHOUT a
#     resolving revision or without evidence BLOCKS;
#   - evidence that does not resolve at the named revision BLOCKS.
#
# No cargo, no daemon, no network; nothing written outside the temp dir.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
CO="$ROOT/scripts/co-order.sh"
[[ -f "$CO" ]] || { echo "cannot find $CO"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
export CO_FEATURES="$T/features"
REV="$(git rev-parse HEAD)"
DOC="docs/ARCHITECTURE_TOUR.md"

rc=0
pass() { echo "  ok    $1"; }
flunk() { echo "  FAIL  $1 — $2"; rc=1; }

# mkorder <id>  — writes the common order body; the rest of stdin is the
# Extension section (so a case can omit it entirely).
mkorder() {
    local id="$1"
    mkdir -p "$CO_FEATURES/$id"
    {
        printf -- '---\nid: %s\nstatus: open\n---\n\n' "$id"
        printf -- '## Objective\nDone when: x y z.\nNot worth continuing if: x.\n\n'
        printf -- '## Demo\n(none)\n\n## Steps\n(none)\n\n## Scope\n- a\n\n'
        printf -- '## Seams\n(none)\n\n'
        cat
    } > "$CO_FEATURES/$id/order.md"
}

# expect <name> <exit> <grep-pattern>
expect() {
    local name="$1" want="$2" pat="$3" out rcx
    set +e; out="$(bash "$CO" check "$name" 2>&1)"; rcx=$?; set -e
    if [ "$rcx" != "$want" ]; then
        flunk "$name" "exit $rcx want $want"; sed 's/^/          /' <<<"$out" | head -10; return
    fi
    if ! grep -q "$pat" <<<"$out"; then
        flunk "$name" "missing '$pat'"; sed 's/^/          /' <<<"$out" | head -10; return
    fi
    pass "$name"
}

echo "extension-decision:"

# 1. No Extension section at all -> pass, nudge.
mkorder absent </dev/null
expect absent 0 "Extension is (none)"

# 2. Template default (all labels (none)) -> pass, nudge.
mkorder unclaimed <<'EOF'
## Extension

revision: (none)
home: (none)
pattern: (none)
delta: (none)
growth: (none)
evidence:
unresolved: (none)
EOF
expect unclaimed 0 "Extension is (none)"

# 3. Small edit: revision + home + delta, growth (none), one evidence line.
mkorder small <<EOF
## Extension

revision: $REV
home: $DOC — the tour file
pattern: same file; follows its existing shape
delta: fix the one stale sentence
growth: (none)
evidence:
- $DOC:10 — the section that must stay true
unresolved: (none)
EOF
expect small 0 "ready:"

# 4. Growth named + evidence -> pass.
mkorder growth <<EOF
## Extension

revision: $REV
home: $DOC — the tour file
pattern: docs/ARCHITECTURE_TOUR.md's existing section shape
delta: add a subsection for the extension record
growth: one new heading in an existing file
evidence:
- $DOC:27 — the rules of engagement paragraph
unresolved: whether the heading belongs in the tour or in SYSTEM_OVERVIEW
EOF
expect growth 0 "ready:"

# 5. Growth named but revision is (none) -> BLOCK.
mkorder no-rev <<'EOF'
## Extension

revision: (none)
home: something
pattern: something
delta: something
growth: a new store
evidence:
- a.txt:1
unresolved: (none)
EOF
expect no-rev 1 "new structure requires a revision-bound record"

# 6. Revision that does not resolve -> BLOCK.
mkorder bad-rev <<'EOF'
## Extension

revision: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
home: something
pattern: something
delta: something
growth: (none)
evidence:
- docs/ARCHITECTURE_TOUR.md:1
unresolved: (none)
EOF
expect bad-rev 1 "does not resolve to a commit"

# 7. Evidence path that does not exist at the revision -> BLOCK.
mkorder bad-path <<EOF
## Extension

revision: $REV
home: something
pattern: something
delta: something
growth: (none)
evidence:
- does-not-exist.txt:1 — a hallucinated citation
unresolved: (none)
EOF
expect bad-path 1 "does not exist at revision"

# 8. Evidence line past end of file -> BLOCK.
mkorder bad-line <<EOF
## Extension

revision: $REV
home: something
pattern: something
delta: something
growth: (none)
evidence:
- $DOC:99999 — a line that cannot exist
unresolved: (none)
EOF
expect bad-line 1 "past end of file"

# 9. Present section but a label is missing -> BLOCK.
mkorder missing-label <<EOF
## Extension

revision: $REV
home: something
pattern: something
growth: (none)
evidence:
- $DOC:10
unresolved: (none)
EOF
expect missing-label 1 "delta:\` is missing"

# 10. Growth named but no evidence -> BLOCK.
mkorder no-evidence <<EOF
## Extension

revision: $REV
home: something
pattern: something
delta: something
growth: a new pass
evidence:
unresolved: (none)
EOF
expect no-evidence 1 "evidence:\` is empty"

# 11. Evidence present but revision is (none) -> pass, nudge (nothing checks it).
mkorder ev-no-rev <<'EOF'
## Extension

revision: (none)
home: something
pattern: something
delta: something
growth: (none)
evidence:
- docs/ARCHITECTURE_TOUR.md:10
unresolved: (none)
EOF
expect ev-no-rev 0 "nothing checks it"

echo
if [ "$rc" = 0 ]; then
    echo "extension-decision: all controls passed"
else
    echo "extension-decision: $rc control(s) FAILED"
fi
exit "$rc"

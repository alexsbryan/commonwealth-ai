#!/usr/bin/env bash
#
# demo-governance.sh — the "living common law" demo for a coliving community.
#
# The story: a house runs on a founding Charter + a year of house-meeting
# Decisions that amend it. Nobody remembers what's actually current. This
# assistant answers from CURRENT law, cites the rule, says so plainly when the
# rules don't cover something, and shows what each rule replaced.
#
# Hardened for OPEN Q&A: governance turns are pinned to a factual-lookup path
# (no misroute), carry an answering discipline (cite-or-abstain, no rambling),
# drop superseded rules' evidence (never answer from dead law), and surface
# supersession provenance only for a decision the answer actually used.
#
# Prereq: a healthy daemon with chat + embed models, and the corpus set up
# (scripts/setup-governance-corpus.sh). Usage: scripts/demo-governance.sh [corpus-id]

set -uo pipefail

CORPUS_ID="${1:-maple-house}"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
IDX="${HOME}/.sovereign/indexes/${CORPUS_ID}"

# Clean wrapper: strip the bootstrap/router diagnostics, keep the answer +
# the supersession-provenance footer.
ask() {
  "$BIN" govern ask "$CORPUS_ID" "$1" 2>&1 \
    | grep -vE '^(Daemon|Chat model|Embed model|Database|Indexes|Corpora|Tools|Router classifier|Atlas|Meta-atlas|Temperature|conversation |> |\[router\])' \
    | grep -v '^[[:space:]]*$'
}

pause() { read -r -p $'\n  ['"$1"$']\n'; }

if [[ ! -f "$IDX/atlas/governance_oplog.jsonl" ]]; then
  echo "✘ Corpus '$CORPUS_ID' isn't set up for governance."
  echo "  Run: scripts/setup-governance-corpus.sh   (install + enrich + seed + resolve)"
  exit 1
fi

clear
cat <<EOF
  ════════════════════════════════════════════════════════════
   MAPLE HOUSE — living common law
  ════════════════════════════════════════════════════════════
   A coliving house runs on a founding Charter plus a year of
   house-meeting Decisions that amend it. Over time, nobody
   remembers what's actually current.

   This assistant answers from CURRENT law — and shows you what
   changed.
EOF

# ── Act 1: the hook — current law + what it replaced ──
pause "ENTER — Q: \"how many nights can a guest stay overnight?\""
echo "  ────────────────────────────────────────────────────────────"
ask "How many nights can a guest stay overnight?"
cat <<'EOF'

  ↑ It didn't quote the founding charter's two-night rule. The house
    VOTED to change it — and the assistant tells you exactly what the
    current decision replaced.
EOF

# ── Act 2: the open floor — ask anything ──
pause "ENTER — open the floor"
cat <<'EOF'
  ────────────────────────────────────────────────────────────
   Ask anything about the house rules. It answers from current
   law and cites the rule — and when the rules don't cover your
   question, it says so plainly instead of making something up.
   (blank line + ENTER to finish)
  ────────────────────────────────────────────────────────────
EOF
while true; do
  printf '\n  ask the house ▷ '
  IFS= read -r q || break
  [[ -z "${q// }" ]] && break
  echo "  ────────────────────────────────────────────────────────────"
  ask "$q"
done

# ── Act 3: how it knows — detected tensions ──
pause "ENTER — how does it know what's current?"
cat <<'EOF'
  ────────────────────────────────────────────────────────────
   Behind the scenes it continuously finds places where a later
   Decision conflicts with the Charter — the "house meeting
   agenda". A human adjudicates each one (keep the decision, or
   accept the tension); from then on the assistant answers from
   the rule that won. Here are the open ones it has surfaced:
  ────────────────────────────────────────────────────────────
EOF
"$BIN" govern tensions "$CORPUS_ID" 2>&1 | grep -vE '^(warning:|$)' | head -24

cat <<'EOF'

  ════════════════════════════════════════════════════════════
   Detect conflicts → adjudicate like a house meeting → answer
   from current law, cited, with provenance. That's the loop.
  ════════════════════════════════════════════════════════════
EOF

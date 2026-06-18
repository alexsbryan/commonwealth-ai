#!/usr/bin/env bash
#
# setup-governance-corpus.sh — one-command, reproducible setup of the FR-9
# governance reference corpus ("Maple House"), installed under the fixed
# corpus_id `maple-house`.
#
# Unlike the chaos corpus (retrieval-only), governance needs the enriched
# ATLAS (rule Claim atoms + Tension edges) AND a governed+resolved oplog:
#   1. install + enrich    — produce atoms.json / edges.json / chapters.json.
#   2. govern seed         — AssertRule every rule-claim (the governed baseline;
#                            nothing else populates the oplog today).
#   3. resolve supersessions — Supersede the two clean charter→decision conflicts
#                            (guests, chores) so the charter rules become dead
#                            law. This is what makes the Lane B SupersededTrap
#                            questions (RL-3) and the active-set retrieval filter
#                            actually fire. Resolution is by RULE-TEXT matching
#                            (atom ids vary per enrichment), so it is reproducible
#                            across machines.
#
# After this, capture the Lane A + Lane B baselines on a healthy daemon:
#   sovereign bench governance run maple-house --out target/gov-a.json
#   sovereign bench gate governance --report target/gov-a.json --update-baseline
#   sovereign bench governance qa  maple-house --out target/gov-b.jsonl
#   sovereign bench gate governance-qa --report target/gov-b.jsonl --update-baseline
#
# Prerequisites: a healthy daemon, `jq`, and (for the enrich step) the chat +
# embed models loaded. The full enrich is slow (~15-20 min, live model).
#
# Usage:  scripts/setup-governance-corpus.sh [--bin <cli>] [--skip-enrich]

set -euo pipefail

CORPUS_ID="maple-house"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL_RECIPE="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/recipe.toml"
CANONICAL_MD="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/maple-house.md"
OVERRIDE_DIR="${HOME}/.sovereign/recipes/${CORPUS_ID}"
OVERRIDE_RECIPE="${OVERRIDE_DIR}/recipe.toml"
IDX="${HOME}/.sovereign/indexes/${CORPUS_ID}"
SKIP_ENRICH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --skip-enrich) SKIP_ENRICH=1; shift ;;
    -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "FATAL: jq is required for reproducible resolution" >&2; exit 2; }
[[ -f "$CANONICAL_RECIPE" ]] || { echo "FATAL: recipe not found at $CANONICAL_RECIPE" >&2; exit 2; }
[[ -f "$CANONICAL_MD" ]] || { echo "FATAL: corpus markdown not found at $CANONICAL_MD" >&2; exit 2; }

# 1. Mirror the committed recipe into the daemon's live override dir, rewriting
#    the source path to the absolute committed markdown for THIS checkout. The
#    running daemon resolves ~/.sovereign/recipes/<id>/recipe.toml first, so no
#    rebuild/restart is needed.
mkdir -p "$OVERRIDE_DIR"
sed "s#^path = .*#path = \"${CANONICAL_MD}\"#" "$CANONICAL_RECIPE" > "$OVERRIDE_RECIPE"
echo "live recipe: $OVERRIDE_RECIPE"
echo "             $(grep '^path' "$OVERRIDE_RECIPE")"

if [[ -z "$SKIP_ENRICH" ]]; then
  # 2. Install + enrich (produces the atlas the governance read-model joins).
  echo "installing corpus '${CORPUS_ID}' …"
  "$BIN" corpus install "$CORPUS_ID"
  echo "initialising enrichment (auto-detects the custom governance ontology) …"
  "$BIN" enrich init "$CORPUS_ID" --from-corpus "$CORPUS_ID"
  echo "building the atlas (full; this is the slow step) …"
  "$BIN" enrich build "$CORPUS_ID" --full
else
  echo "── --skip-enrich: reusing the existing atlas at $IDX/atlas ──"
fi

[[ -f "$IDX/atlas/atoms.json" ]] || {
  echo "FATAL: no enriched atlas at $IDX/atlas — run without --skip-enrich first." >&2
  exit 1
}

# 3. Seed the governed rule baseline (idempotent AssertRule per Claim atom).
echo "seeding governed rules …"
"$BIN" govern seed "$CORPUS_ID"

# 4. Resolve the two CLEAN full supersessions so the charter rules become dead
#    law. Resolution is by rule-text signature (verbatim from maple-house.md),
#    so the right decision wins regardless of the enrichment's atom ids. If the
#    detector missed a tension, we warn rather than fail (the Lane A bench
#    measures that recall separately).
resolve_supersession() {
  local label="$1" old_sig="$2" new_sig="$3"
  local json
  json="$("$BIN" govern tensions "$CORPUS_ID" --format json 2>/dev/null || echo '[]')"
  local pair
  pair="$(printf '%s' "$json" | jq -r --arg old "$old_sig" --arg new "$new_sig" '
    [ .[]
      | (.text_a + " ||| " + .text_b | ascii_downcase) as $both
      | select(($both | contains($old | ascii_downcase)) and ($both | contains($new | ascii_downcase)))
      | [ .id, (if (.text_a | ascii_downcase | contains($new | ascii_downcase)) then .rule_a else .rule_b end) ]
    ] | .[0] // [] | @tsv')"
  local tid keep
  IFS=$'\t' read -r tid keep <<<"$pair"
  if [[ -z "${tid:-}" || -z "${keep:-}" ]]; then
    echo "⚠ $label: no open tension matched signatures [\"$old_sig\" × \"$new_sig\"] — detector may have missed it; skipping."
    return 0
  fi
  echo "resolving $label: tension $tid, keep $keep (supersedes the charter rule)"
  "$BIN" govern resolve "$CORPUS_ID" "$tid" --keep "$keep" --rationale "setup: $label — later decision supersedes the charter rule"
}

# NOTE: signatures match the *extracted claim* text (an LLM paraphrase of the
# source), not the raw markdown — so they can drift if a re-enrichment phrases a
# rule differently. If a resolve reports "no open tension matched", run
# `govern tensions maple-house` and resolve the guest/chore supersessions by id
# (keep the dated Decision over the Charter Article).
resolve_supersession "guests" "two consecutive nights" "any number of nights"
resolve_supersession "chores" "exempt from the rotation for any reason" "more than two consecutive weeks"

echo
echo "✓ governance corpus ready: $IDX"
"$BIN" govern tensions "$CORPUS_ID" | sed -n '1,3p' || true
echo
echo "Verify the headline:"
echo "    $BIN govern ask $CORPUS_ID \"how many nights can a guest stay?\""
echo "    → should answer from the active rule (overnight guests not permitted),"
echo "      NOT the two-night charter, with a supersession-provenance line."
echo
echo "Run the FR-9 lanes:"
echo "    $BIN bench governance run $CORPUS_ID --out target/gov-a.json      # Lane A (detector)"
echo "    $BIN bench governance qa  $CORPUS_ID --out target/gov-b.jsonl     # Lane B (Q&A, RL-1/2/3)"

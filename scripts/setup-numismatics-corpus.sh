#!/usr/bin/env bash
#
# setup-numismatics-corpus.sh — run the ontology-v1 chain end to end and
# ASSERT that the author's declared nouns come out the other end.
#
# The objective of ontology-v1 is one sentence: a domain expert writes TOML,
# and their own nouns come out. Every command in that chain had unit tests and
# the chain had never once been run, so three breaks lived in the seams between
# commands where no unit test looks (2026-09-02: `corpus install` refused the
# path `recipe validate` had just accepted; a relative acquire path resolved
# against the daemon's cwd; a section the pipeline skipped by its own word
# floor stopped the build at step 1 of 9). This script is the standing version
# of that run.
#
# What it proves, in order:
#   1. recipe validate      — the declaration parses, and prints what it derives
#   2. corpus install <path>— the file the author validated installs, by path
#   3. enrich init          — the ontology reaches the enrichment config
#   4. enrich build --full  — a live model extracts, resolves and backfills
#   5. atoms.json           — THE PAYLOAD. Atoms carry `coin` / `sceatta` /
#                             `ruler` / `mint`, not the six generic kinds.
#
# Step 5 is the only one that speaks to the objective. Steps 1-4 exiting 0 is
# how the chain LOOKED healthy while nothing had been demonstrated.
#
# Prerequisites: a healthy daemon with the chat + embed models loaded, and jq.
# The build step is slow (~25-40 min against a live model).
#
# Usage:  scripts/setup-numismatics-corpus.sh [--bin <cli>] [--skip-enrich]
#         scripts/setup-numismatics-corpus.sh --assert-only   # step 5 alone

set -euo pipefail

CORPUS_ID="wessex-hoard"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RECIPE="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/recipe.toml"
IDX="${HOME}/.svrnmesh/indexes/${CORPUS_ID}"
ATOMS="${IDX}/atlas/atoms.json"
SKIP_ENRICH=""
ASSERT_ONLY=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --skip-enrich) SKIP_ENRICH=1; shift ;;
    --assert-only) ASSERT_ONLY=1; SKIP_ENRICH=1; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "FATAL: jq is required" >&2; exit 2; }
[[ -f "$RECIPE" ]] || { echo "FATAL: recipe not found at $RECIPE" >&2; exit 2; }

if [[ -z "$ASSERT_ONLY" ]]; then
  echo "── 1. validate ──"
  "$BIN" recipe validate "$RECIPE"

  if [[ -z "$SKIP_ENRICH" ]]; then
    # By PATH, not by id. `corpus install` registers the file under the id the
    # recipe declares and resolves the relative acquire path against the
    # recipe's own directory — which is why this script needs no `sed` rewrite
    # of the committed recipe, unlike setup-governance-corpus.sh.
    echo "── 2. install ──"
    "$BIN" corpus install "$RECIPE" --wait=900

    echo "── 3. enrich init ──"
    "$BIN" enrich init "$CORPUS_ID" --from-corpus "$CORPUS_ID"

    echo "── 4. enrich build (slow: live model) ──"
    "$BIN" enrich build "$CORPUS_ID" --full
  else
    echo "── --skip-enrich: reusing the atlas at $IDX/atlas ──"
  fi
fi

echo "── 5. the payload: are the author's nouns in the atoms? ──"
[[ -f "$ATOMS" ]] || {
  echo "FATAL: no atlas at $ATOMS — run without --skip-enrich first." >&2
  exit 1
}

# The five declared types, from sovereign-recipes/wessex-hoard/recipe.toml.
# Read from the recipe rather than repeated here, so a template change cannot
# leave this assertion quietly checking a stale vocabulary (§10.6).
mapfile -t DECLARED < <(grep -A1 '^\[\[enrichment.ontology.types\]\]' "$RECIPE" \
  | grep '^name = ' | sed 's/name = "\(.*\)"/\1/')
(( ${#DECLARED[@]} > 0 )) || { echo "FATAL: read no declared types out of $RECIPE" >&2; exit 1; }
echo "declared: ${DECLARED[*]}"

total=$(jq '.atoms | length' "$ATOMS")
echo "atoms: $total"

fail=0
declared_total=0
for t in "${DECLARED[@]}"; do
  # A declared type can land on any atom kind — `coin` is an entity type,
  # `attribution` a claim type — so look at every type-bearing field rather
  # than assuming which one the declaration turned into.
  n=$(jq --arg t "$t" '[.atoms[] | select(
        (.data.entity_type // empty) == $t
     or (.data.claim_kind  // empty) == $t
     or (.data.relation_type // empty) == $t
     or (.data.event_type  // empty) == $t
     or (.data.state_type  // empty) == $t)] | length' "$ATOMS")
  declared_total=$(( declared_total + n ))
  printf '  %-14s %s\n' "$t" "$n"
done

# The bar. Not "some atoms exist" — atoms carrying a name the AUTHOR wrote.
# A build that produced 200 atoms and zero declared types is the exact failure
# this whole program exists to prevent, and it exits 0 everywhere else.
if (( declared_total == 0 )); then
  echo >&2
  echo "FAIL: $total atom(s), NOT ONE carrying a declared type." >&2
  echo "      Every atom fell back to the six generic kinds. The declaration" >&2
  echo "      reached the config and did not reach the atoms." >&2
  echo "      What it produced instead:" >&2
  jq -r '[.atoms[].data | (.entity_type // .claim_kind // .relation_type // .event_type // .state_type // "untyped")]
         | group_by(.) | map("        \(.[0]): \(length)") | .[]' "$ATOMS" >&2
  exit 1
fi

echo
echo "PASS: $declared_total of $total atom(s) carry a type the author declared."

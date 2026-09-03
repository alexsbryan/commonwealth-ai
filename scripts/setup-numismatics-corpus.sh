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
#   5. atoms.json           — THE PAYLOAD, scored against
#                             sovereign-recipes/wessex-hoard/truth.json: every
#                             catalogued coin present under its declared
#                             identity key, the enumeration probe's count over
#                             the declared family, every named mint and ruler,
#                             one claim per labelled attribution.
#
# Step 5 is the only one that speaks to the objective. Steps 1-4 exiting 0 is
# how the chain LOOKED healthy while nothing had been demonstrated.
#
# STEPS 1-4 ARE NO LONGER THIS SCRIPT'S CLAIM. They are declared as the
# `ontology-author` journey in sovereign/docs/cli-contract.toml, so `svrn
# contract census` and pre-push can see them and a step that stops asserting
# turns a lane red. They stay here because this is also how the hoard gets
# REBUILT, and a rebuild that skipped its own chain would be a second path.
# What is this script's own, and belongs nowhere else, is step 5's recall
# table: a bar per truth row is not a `stdout_contains`.
#
# It reports FOUR verdicts, not two (ARCH §18.2), because until 2026-09-03 it
# reported one: it passed whenever a SINGLE atom carried a declared type — the
# bar failed on 1 of the 176 shapes an atlas can take — and `--assert-only`
# never asked whether the atlas in front of it was built from the declaration
# in front of it, so a stale atlas printed PASS.
#
#   exit 0  PASS             every bar in truth.json is met
#   exit 1  FAIL / NEVER-RAN a bar missed, or there is no atlas to judge
#   exit 3  COULD-NOT-JUDGE  the atlas was built from a different declaration,
#                            or the recipe/source is newer than the atlas
#   exit 2  FATAL            the harness itself cannot run (no jq, no recipe)
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
ONTOLOGY="${IDX}/atlas/ontology.json"
TRUTH="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/truth.json"
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

# ── Provenance first. Four verdicts, not two (ARCH §18.2) ────────────────────
#
# Until 2026-09-03 this step had two: it printed PASS whenever ONE atom carried
# a declared type, and `--assert-only` never asked whether the atlas in front of
# it was built from the declaration in front of it. So a run against a stale
# atlas — or an atlas built before the recipe was edited — printed the same PASS
# as a fresh one. NEVER-RAN and COULD-NOT-JUDGE are their own exits now.
[[ -f "$ATOMS" ]] || {
  echo "NEVER-RAN: no atlas at $ATOMS. Run without --skip-enrich first." >&2
  exit 1
}
[[ -f "$ONTOLOGY" ]] || {
  echo "NEVER-RAN: $IDX/atlas has atoms but no ontology.json — the atlas was" >&2
  echo "           built before the declaration reached it." >&2
  exit 1
}
[[ -f "$TRUTH" ]] || { echo "FATAL: no ground truth at $TRUTH" >&2; exit 2; }

# The declaration the BUILD used, against the one on disk now. Two readings of
# the staleness question, because neither alone is enough: the type names are
# compared structurally (a renamed or added type is caught exactly), and the
# recipe's mtime catches every other edit — a grade, an attribute, an identity
# key — without a TOML parser this script does not have.
built_names=$(jq -r '[.policies.shape.types[].name] | sort | join(",")' "$ONTOLOGY")
recipe_names=$(grep -A1 '^\[\[enrichment.ontology.types\]\]' "$RECIPE" \
  | sed -n 's/^name = "\(.*\)"/\1/p' | sort | paste -sd, -)
if [[ "$built_names" != "$recipe_names" ]]; then
  echo "COULD-NOT-JUDGE: the atlas was built from a DIFFERENT declaration." >&2
  echo "  built:  $built_names" >&2
  echo "  recipe: $recipe_names" >&2
  echo "  Rebuild before asserting: $BIN enrich build $CORPUS_ID --full" >&2
  exit 3
fi

mtime() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1"; }
if (( $(mtime "$RECIPE") > $(mtime "$ONTOLOGY") )); then
  echo "COULD-NOT-JUDGE: $RECIPE was edited after the atlas was built." >&2
  echo "  recipe:  $(date -r "$RECIPE" 2>/dev/null || true)" >&2
  echo "  atlas:   $(date -r "$ONTOLOGY" 2>/dev/null || true)" >&2
  exit 3
fi

# The corpus text under it. An atlas older than its own source is not evidence
# about that source.
SRC_REL=$(sed -n '/^\[acquire\]/,/^\[/p' "$RECIPE" | sed -n 's/^path = "\(.*\)"/\1/p' | head -1)
SRC="${SRC_REL}"
[[ "$SRC" = /* ]] || SRC="$(dirname "$RECIPE")/${SRC_REL}"
if [[ -f "$SRC" ]] && (( $(mtime "$SRC") > $(mtime "$ATOMS") )); then
  echo "COULD-NOT-JUDGE: $SRC is newer than the atlas built from it." >&2
  exit 3
fi

# `_summary.json` records the atoms.json it was written for. When it disagrees
# the summary describes a different atlas than the one being asserted on — say
# so rather than quietly reading either.
SUMMARY="${IDX}/atlas/_summary.json"
if [[ -f "$SUMMARY" ]]; then
  s_size=$(jq -r '.atoms_size_bytes // empty' "$SUMMARY")
  a_size=$(wc -c < "$ATOMS" | tr -d ' ')
  printf 'atlas: %s atoms, fingerprint %s\n' \
    "$(jq -r '.atom_count // "?"' "$SUMMARY")" "$(jq -r '.fingerprint // "?"' "$SUMMARY")"
  if [[ -n "$s_size" && "$s_size" != "$a_size" ]]; then
    echo "WARN: _summary.json describes a ${s_size}-byte atoms.json; this one is ${a_size}." >&2
    echo "      The summary is stale — its counts are not this atlas's." >&2
  fi
fi

# ── The bars, read from truth.json ───────────────────────────────────────────
#
# Not "some atom carried a declared type" — that bar failed on 1 of the 176
# shapes an atlas can take, so 175 wrong atlases passed it. These are recall
# bars against the exhaustively-labelled manifest the eval bank and the
# retrieval e2e test both read. Over-production is not failed here: the corpus
# holds articles about coins outside the catalogue, so counts exceed truth and
# the question is whether every catalogued thing LANDED.

total=$(jq '.atoms | length' "$ATOMS")
echo "atoms: $total"
echo

fail=0
bar() {  # bar <name> <got> <want> <detail>
  local status="ok"
  (( $2 >= $3 )) || { status="MISSED"; fail=1; }
  printf '  %-22s %2s / %-2s  %-7s %s\n' "$1" "$2" "$3" "$status" "${4:-}"
}

# 1. IDENTITY — every catalogued coin reached the atlas under its declared
#    identity key. This is the bar the whole identity arc exists to move.
want_refs=$(jq -r '[.entities.coin[].catalogue_ref] | length' "$TRUTH")
missing_refs=$(jq -r --slurpfile t "$TRUTH" '
  ([$t[0].entities.coin[].catalogue_ref]) as $want
  | ([.atoms[].data.attributes.catalogue_ref] | map(select(. != null))) as $got
  | ($want - $got) | join(", ")' "$ATOMS")
got_refs=$(( want_refs - $(jq -r --slurpfile t "$TRUTH" '
  ([$t[0].entities.coin[].catalogue_ref]) as $want
  | ([.atoms[].data.attributes.catalogue_ref] | map(select(. != null))) as $got
  | ($want - $got) | length' "$ATOMS") ))
bar "catalogue_ref" "$got_refs" "$want_refs" "${missing_refs:+missing: $missing_refs}"

# 2. FAMILY — the enumeration probe's count, over the declared type and every
#    type that specializes it. The family comes from the declaration, not from
#    a list repeated here.
want_coins=$(jq -r '.enumeration_probe.expected_coin_count' "$TRUTH")
family=$(jq -r '[.policies.shape.types[] | select(.name == "coin" or .specializes == "coin") | .name]' "$ONTOLOGY")
got_coins=$(jq --argjson fam "$family" '[.atoms[] | select(.data.entity_type as $t | $fam | index($t))] | length' "$ATOMS")
bar "coin family" "$got_coins" "$want_coins" "$(echo "$family" | jq -r 'join(" + ")')"

# 3+4. The other counted types, by NAME. A mint or ruler the catalogue names
#      and the atlas does not is a miss no total can hide.
for kind in mint ruler; do
  want=$(jq -r --arg k "$kind" '[.entities[$k][].name] | length' "$TRUTH")
  if [[ "$kind" == ruler ]]; then
    # `ruler` declares `role_of = "person"`, so it lands as a State on a
    # person atom — a part played is not an essence (ARCH §7.5).
    names=$(jq -r '[.atoms[] | select(.data.state_type == "ruler") | .data.entity_id] as $ids
                   | [.atoms[] | select(.data.id as $i | $ids | index($i)) | .data.canonical_name]' "$ATOMS")
  else
    names=$(jq -r --arg k "$kind" '[.atoms[] | select(.data.entity_type == $k) | .data.canonical_name]' "$ATOMS")
  fi
  missing=$(jq -r --slurpfile t "$TRUTH" --arg k "$kind" --argjson got "$names" '
    [$t[0].entities[$k][].name] | map(select(. as $n | ($got | map(contains($n)) | any) | not)) | join(", ")' <<<'null')
  got=$(( want - $(jq -r --slurpfile t "$TRUTH" --arg k "$kind" --argjson got "$names" '
    [$t[0].entities[$k][].name] | map(select(. as $n | ($got | map(contains($n)) | any) | not)) | length' <<<'null') ))
  bar "$kind" "$got" "$want" "${missing:+missing: $missing}"
done

# 5. ATTRIBUTIONS — the claim type, at least one per labelled attribution.
want_attr=$(jq -r '.attributions | length' "$TRUTH")
got_attr=$(jq '[.atoms[] | select(.data.claim_kind == "attribution")] | length' "$ATOMS")
bar "attribution" "$got_attr" "$want_attr"

# GRADES — reported, and only a TOTAL failure blocks. The declared enum is the
# author's; which of its values an extraction reaches is a quality measure, not
# a chain break. Measured 2026-09-03 on the built atlas: 3 of 4 — `die-link`
# never lands, on 49 attribution claims of which 35 carry no grade at all.
declared_grades=$(jq -r '[.policies.shape.types[] | select(.kind == "claim") | .grades // []] | add // []' "$ONTOLOGY")
seen_grades=$(jq -r '[.atoms[] | select(.data.claim_kind == "attribution") | .data.attributes.grade // .data.grade] | map(select(. != null)) | unique' "$ATOMS")
absent=$(jq -rn --argjson d "$declared_grades" --argjson s "$seen_grades" '($d - $s) | join(", ")')
n_seen=$(jq -rn --argjson d "$declared_grades" --argjson s "$seen_grades" '($d - ($d - $s)) | length')
bar "grade values" "$(( n_seen > 0 ? 1 : 0 ))" 1 \
  "$n_seen of $(jq -rn --argjson d "$declared_grades" '$d | length') declared${absent:+ — never extracted: $absent}"

echo
if (( fail )); then
  echo "FAIL: the declaration reached the config and did not reach the atoms." >&2
  echo "      What the atlas produced instead:" >&2
  jq -r '[.atoms[].data | (.entity_type // .claim_kind // .relation_type // .event_type // .state_type // "untyped")]
         | group_by(.) | map("        \(.[0]): \(length)") | .[]' "$ATOMS" >&2
  exit 1
fi

echo "PASS: every bar in $TRUTH is met by the atlas at $ATOMS."

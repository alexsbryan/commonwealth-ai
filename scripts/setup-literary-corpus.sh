#!/usr/bin/env bash
#
# setup-literary-corpus.sh — derive, VERIFY, and install the `brothers-karamazov-book-1`
# bench corpus: Dostoevsky's *The Brothers Karamazov*, Book I ("The History Of A
# Family", chapters I-V), from Project Gutenberg #28054.
#
# This corpus backs the `literary/bk-book-1` enrichment lane in
# scripts/sovereign-ci-bench.sh — the HARD, baseline-diffed gate that scores a
# resolved literary atlas against sovereign/bench/literary/bk-book-1.toml.
#
# YOU DO NOT NEED THIS SCRIPT TO INSTALL. Unlike setup-chaos-corpus.sh, the
# recipe's `[acquire]` points at an https URL rather than a $HOME path, and the
# recipe is registered in sovereign-recipes/registry.toml, so the plain
#
#     svrn corpus install brothers-karamazov-book-1
#
# works on a clean checkout. What this script adds is PROVENANCE: it re-derives
# the source text from Project Gutenberg and fails loudly if the bytes differ
# from what we published. Run it when you want to verify the hosted artifact
# rather than trust it, or when Gutenberg re-issues the text and you need to
# know that the bench input moved.
#
# WHY BOOK I AND NOT THE WHOLE NOVEL. The golden is built on leakage
# anti-tests: `forbidden_event_atoms` for "Mitya's trial" (Book XII) and
# `forbidden_relation_atoms` for Alyosha/Lise/Grushenka/Katerina (Books II+).
# Extract the full novel and each of those inverts — the extractor gets
# penalised for correctly reading the text. The scoping must live in the SOURCE
# DOCUMENT, not in a flag, because the weekly `--rebuild` tier shells
# `svrn enrich build <corpus_id>` with no chapter selection
# (sovereign-cli-llm/src/bench_cmd/all.rs::rebuild_corpus).
#
# Usage:  scripts/setup-literary-corpus.sh [--bin <cli>] [--verify-only] [--mirror-recipe]

set -euo pipefail

CORPUS_ID="brothers-karamazov-book-1"
DIR="${HOME}/.sovereign/bench-corpora/${CORPUS_ID}"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
SRC_NAME="brothers-karamazov-book-1.txt"

# Project Gutenberg #28054, Constance Garnett translation. Public domain in the US.
PG_URL="https://www.gutenberg.org/cache/epub/28054/pg28054.txt"

# Book I spans line 175 ("Book I. The History Of A Family") through line 1288;
# line 1289 is the "Book II. An Unfortunate Gathering" heading. These are line
# numbers in the canonical Gutenberg plaintext, verified 2026-08-07.
BOOK1_FIRST_LINE=175
BOOK1_LAST_LINE=1288

# sha256 of the derived slice. This is the contract: the bench scores an atlas
# extracted from EXACTLY these bytes, and the copy hosted on HuggingFace is a
# byte-identical mirror of them. A mismatch is a real signal, never something to
# paper over — see the failure message below.
EXPECTED_SHA="cddb992e50a21c3dd4ba5da5e205722f1c568ee315e93bc0437788f64b1b81b6"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL_RECIPE="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/recipe.toml"
OVERRIDE_RECIPE="${HOME}/.sovereign/recipes/${CORPUS_ID}/recipe.toml"

VERIFY_ONLY=""
MIRROR_RECIPE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --verify-only) VERIFY_ONLY=1; shift ;;
    --mirror-recipe) MIRROR_RECIPE=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else sha256sum "$1" | awk '{print $1}'; fi
}

# ── 1. Fetch the canonical Gutenberg text ───────────────────────────────────
mkdir -p "$DIR"
PG_RAW="${DIR}/pg28054.txt"
if [[ ! -s "$PG_RAW" ]]; then
  echo "fetching Project Gutenberg #28054 → $PG_RAW"
  curl -fsS --max-time 180 "$PG_URL" -o "$PG_RAW"
else
  echo "using cached Gutenberg text: $PG_RAW"
fi

# ── 2. Derive the Book I slice ──────────────────────────────────────────────
# Guard the line offsets by CONTENT, not position alone: if Gutenberg re-flows
# the file, the line numbers silently point at the wrong prose and the sha check
# below would be the only thing that catches it. Checking the headings names the
# failure precisely instead of just reporting "hash differs".
# The Gutenberg plaintext is CRLF-terminated, and so is the published slice
# (the sha256 below is over CRLF bytes). Strip the trailing CR for the heading
# comparison only — never from the derived file, or the hash will not match.
first_seen="$(sed -n "${BOOK1_FIRST_LINE}p" "$PG_RAW")"; first_seen="${first_seen%$'\r'}"
after_last="$(sed -n "$((BOOK1_LAST_LINE + 1))p" "$PG_RAW")"; after_last="${after_last%$'\r'}"
if [[ "$first_seen" != "Book I. The History Of A Family" ]]; then
  echo "FATAL: line ${BOOK1_FIRST_LINE} is not the Book I heading." >&2
  echo "  expected: 'Book I. The History Of A Family'" >&2
  echo "  found:    '${first_seen}'" >&2
  echo "  Gutenberg has re-issued #28054. Re-derive the offsets and re-publish;" >&2
  echo "  do NOT adjust EXPECTED_SHA without re-minting the bench baseline." >&2
  exit 1
fi
if [[ "$after_last" != "Book II. An Unfortunate Gathering" ]]; then
  echo "FATAL: line $((BOOK1_LAST_LINE + 1)) is not the Book II heading." >&2
  echo "  expected: 'Book II. An Unfortunate Gathering'" >&2
  echo "  found:    '${after_last}'" >&2
  exit 1
fi

SRC="${DIR}/${SRC_NAME}"
sed -n "${BOOK1_FIRST_LINE},${BOOK1_LAST_LINE}p" "$PG_RAW" > "$SRC"
echo "derived Book I: $(wc -l < "$SRC" | tr -d ' ') lines, $(wc -w < "$SRC" | tr -d ' ') words → $SRC"

# ── 3. Verify against the published contract ────────────────────────────────
ACTUAL_SHA="$(sha256_of "$SRC")"
if [[ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]]; then
  echo "FATAL: derived Book I does not match the published source text." >&2
  echo "  expected sha256: $EXPECTED_SHA" >&2
  echo "  actual   sha256: $ACTUAL_SHA" >&2
  echo >&2
  echo "  The bench baseline at sovereign/bench/literary/baselines/bk-book-1/ was" >&2
  echo "  minted from an atlas extracted from the EXPECTED bytes. Scoring a" >&2
  echo "  different text against it would report a model regression that is" >&2
  echo "  really a corpus change. Resolve the drift before running the lane." >&2
  exit 1
fi
echo "sha256 verified: $ACTUAL_SHA"
echo "  (matches https://huggingface.co/datasets/svrnmesh/brothers-karamazov-book-1)"

if [[ -n "$VERIFY_ONLY" ]]; then
  echo "--verify-only: source text confirmed, skipping install."
  exit 0
fi

# ── 4. Optionally mirror the recipe into the daemon's live override dir ─────
# Only needed when the RUNNING daemon predates this recipe being added to
# sovereign-recipes/. The bundled recipe is vendored at COMPILE time
# (corpus-engine/build.rs), so a binary built before this recipe landed cannot
# resolve it — and resolution step 2 errors with "No registry entry" before it
# can reach the bundled fallback. Mirroring to ~/.sovereign/recipes/ hits
# resolution step 1 and needs neither a rebuild nor a restart.
if [[ -n "$MIRROR_RECIPE" ]]; then
  if [[ ! -f "$CANONICAL_RECIPE" ]]; then
    echo "FATAL: canonical recipe not found at $CANONICAL_RECIPE" >&2
    exit 2
  fi
  mkdir -p "$(dirname "$OVERRIDE_RECIPE")"
  cp "$CANONICAL_RECIPE" "$OVERRIDE_RECIPE"
  echo "live recipe mirrored: $OVERRIDE_RECIPE"
fi

# ── 5. Install via the running daemon ───────────────────────────────────────
[[ -x "$BIN" ]] || { echo "FATAL: CLI not found/executable at $BIN (build it first)" >&2; exit 2; }
echo "installing corpus '${CORPUS_ID}' …"
"$BIN" corpus install "$CORPUS_ID"

# ── 6. Wait for the canonical index ─────────────────────────────────────────
IDX="${HOME}/.sovereign/indexes/${CORPUS_ID}"
echo -n "waiting for ingest"
landed=""
for _ in $(seq 1 60); do
  if [[ -e "$IDX/chunks.lance" ]]; then landed=1; echo " — done."; break; fi
  echo -n "."
  sleep 5
done

if [[ -z "$landed" ]]; then
  echo
  # A stranded partition means THE INGEST FAILED — it is not a promotion bug.
  #
  # All new ingests write to `<id>-partition-<node>/`; `finalise_solo_ingest`
  # renames it to canonical, and it is reached ONLY on the `Ok` arm
  # (engine/ingest.rs). So any error after the index-build phase leaves a
  # fully-built partition with no canonical — while `corpus install` still
  # exits 0 and prints "spawned". The only evidence is a WARN in
  # ~/.sovereign/logs/daemon.err (NOT daemon.log, which is connection noise).
  #
  # Root-caused 2026-08-07: this recipe declared `[enrichment] type =
  # "field_model"` with `domain = "literary"`. Install only skips the
  # field-model engine for `type = "atlas"`, so it fell through to
  # `FieldModelEngine::from_recipe`, which does not know `literary` and
  # returned `UnknownEnrichmentDomain`. Fixed in the recipe; with
  # `type = "atlas"` the same ingest promotes itself in 4s.
  #
  # This branch stays as a net, but treat it as a SYMPTOM REPORT, not a fix:
  # merging a failed ingest's partition produces a canonical corpus that
  # silently skipped whatever the ingest died on. Read daemon.err before
  # trusting the result.
  if compgen -G "${HOME}/.sovereign/indexes/${CORPUS_ID}-partition-"* >/dev/null; then
    echo "WARNING: ingest landed in a partition and was never promoted."
    echo "  That means the INGEST FAILED after the index-build phase."
    echo "  Check the real error first:  grep -i '\''ingest failed'\'' ~/.sovereign/logs/daemon.err | tail -3"
    echo "  (daemon.log is connection noise; daemon.err carries tracing.)"
    echo "recovering the built chunks with: $BIN corpus merge-partitions $CORPUS_ID --yes"
    echo "  NOTE: this recovers CHUNKS ONLY — whatever the ingest died on did not run."
    "$BIN" corpus merge-partitions "$CORPUS_ID" --yes
  else
    echo "FATAL: no canonical index and no partition at $IDX after ~5min." >&2
    echo "  Check the daemon is healthy: $BIN doctor" >&2
    exit 1
  fi
fi

# ── 7. Repair the enrichment source_path after a prebuilt restore ───────────
# `enrich init` records an ABSOLUTE source path in config.json, and the
# snapshot carries that file verbatim — so a restored corpus points at the
# PUBLISHER's $HOME (verified 2026-08-07: a restore on this box still read
# `/Users/alexsbryan/...`). Chunks, atlas and retrieval are all fine; the one
# thing that breaks is `svrn enrich build`, i.e. the bench's `--rebuild` tier,
# which would fail on a path that does not exist here. Same class of problem
# setup-chaos-corpus.sh solves by rewriting its recipe path for the local $HOME.
ENR_CFG="${HOME}/.sovereign/enrichment/${CORPUS_ID}/config.json"
if [[ -f "$ENR_CFG" ]]; then
  recorded="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('source_path',''))" "$ENR_CFG" 2>/dev/null || echo "")"
  if [[ -n "$recorded" && "$recorded" != "$SRC" ]]; then
    if [[ -e "$recorded" ]]; then
      echo "note: enrichment source_path is '$recorded' (exists) — leaving it alone."
    else
      echo "repairing enrichment source_path for this machine:"
      echo "  was: $recorded  (does not exist here — publisher's \$HOME)"
      echo "  now: $SRC"
      python3 - "$ENR_CFG" "$SRC" <<'PY'
import json, sys
cfg_path, src = sys.argv[1], sys.argv[2]
with open(cfg_path) as fh:
    cfg = json.load(fh)
cfg["source_path"] = src
with open(cfg_path, "w") as fh:
    json.dump(cfg, fh, indent=2)
    fh.write("\n")
PY
    fi
  fi
fi

echo
echo "corpus ready. Verify:"
echo "  $BIN corpus diag $CORPUS_ID"
echo "  $BIN bench all --filter literary/bk-book-1"
echo
echo "NOTE: a fresh install without a [prebuilt] snapshot has chunks but NO"
echo "atlas — the enrichment lane will report the corpus unindexed until you"
echo "build one:  $BIN enrich build $CORPUS_ID"

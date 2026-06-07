#!/usr/bin/env bash
#
# setup-chaos-corpus.sh — one-command, reproducible, MACHINE-STABLE setup of the
# chaos-monkey reference corpus (Conrad, *The Secret Agent*, Project Gutenberg
# #974), installed under the fixed corpus_id `chaos-secret-agent`.
#
# Why a recipe install (not `corpus watch`): `corpus watch` derives the
# corpus_id from the *path hash* (→ a per-machine `watched-<hash>` id), which
# made the CI gate non-reproducible. A recipe pins `[corpus].id`, so every box
# gets the same `chaos-secret-agent` id. The canonical recipe is committed at
# sovereign-recipes/chaos-secret-agent/recipe.toml; this script mirrors it into
# the daemon's live override dir (~/.sovereign/recipes/) with a $HOME-correct
# source path, so the *running* daemon resolves it (registry resolution step 1)
# without a rebuild or restart.
#
# Prerequisites: a healthy daemon, and `yield_to_foreground_secs < 30` in
# ~/.sovereign/config.toml (otherwise the 30s health-ping starves the embed
# pipeline and ingest never completes — see chaos_monkey/README.md).
#
# Usage:  scripts/setup-chaos-corpus.sh [--bin <cli>]

set -euo pipefail

CORPUS_ID="chaos-secret-agent"
DIR="${HOME}/.sovereign/bench-corpora/${CORPUS_ID}"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
URL="https://www.gutenberg.org/cache/epub/974/pg974.txt"
# The canonical recipe in-repo, relative to the repo root (this script's ../).
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL_RECIPE="${REPO_ROOT}/sovereign-recipes/${CORPUS_ID}/recipe.toml"
OVERRIDE_RECIPE="${HOME}/.sovereign/recipes/${CORPUS_ID}/recipe.toml"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Warn if the daemon's yield window would starve ingest.
yld=$(grep -oE "yield_to_foreground_secs *= *[0-9]+" "$HOME/.sovereign/config.toml" 2>/dev/null | grep -oE "[0-9]+$" || echo "")
if [[ -n "$yld" ]] && (( yld >= 30 )); then
  echo "⚠ yield_to_foreground_secs=$yld ≥ 30 — the 30s health-ping will starve ingest."
  echo "  Set it below 30 (e.g. 15) in ~/.sovereign/config.toml and restart the daemon first."
fi

# 1. Fetch the source text to a stable $HOME path (not /tmp).
mkdir -p "$DIR"
if [[ ! -s "$DIR/secret-agent.txt" ]]; then
  echo "fetching The Secret Agent → $DIR/secret-agent.txt"
  curl -fsS --max-time 120 "$URL" -o "$DIR/secret-agent.txt"
fi
echo "corpus text: $(wc -l < "$DIR/secret-agent.txt") lines at $DIR/secret-agent.txt"

# 2. Mirror the committed recipe into the daemon's live override dir, rewriting
#    the absolute source path for THIS machine's $HOME. The running daemon reads
#    ~/.sovereign/recipes/<id>/recipe.toml first (registry resolution step 1),
#    so no rebuild/restart is needed.
if [[ ! -f "$CANONICAL_RECIPE" ]]; then
  echo "FATAL: canonical recipe not found at $CANONICAL_RECIPE" >&2
  exit 2
fi
mkdir -p "$(dirname "$OVERRIDE_RECIPE")"
sed "s#^path = .*#path = \"$DIR/secret-agent.txt\"#" "$CANONICAL_RECIPE" > "$OVERRIDE_RECIPE"
echo "live recipe: $OVERRIDE_RECIPE"
echo "             $(grep '^path' "$OVERRIDE_RECIPE")"

# 3. Install via the running daemon (resolves the override recipe by id).
echo "installing corpus '${CORPUS_ID}' …"
"$BIN" corpus install "$CORPUS_ID"

# 4. Wait for the canonical index to land (ingest is ~41s + a slow start while
#    the embed pipeline warms up). The index dir appears once promotion to
#    canonical completes.
IDX="${HOME}/.sovereign/indexes/${CORPUS_ID}"
echo -n "waiting for ingest"
for _ in $(seq 1 60); do
  if [[ -e "$IDX/chunks.lance" ]]; then
    echo " — done."
    break
  fi
  echo -n "."
  # bounded delay without a foreground sleep dependency
  curl -s --max-time 5 "http://127.0.0.1:9742/internal/corpus/progress" >/dev/null 2>&1 || true
done

if [[ -e "$IDX/chunks.lance" ]]; then
  echo "✓ installed: $IDX"
  echo
  echo "Run the chaos bench against the stable corpus:"
  echo "    $BIN bench chaos-monkey run \\"
  echo "      --bank sovereign/bench/chaos_monkey/secret_agent.toml \\"
  echo "      --manifest sovereign/bench/chaos_monkey/manifest.toml \\"
  echo "      --corpus ${CORPUS_ID}"
  echo
  echo "Or the whole CI suite (the chaos lane defaults CHAOS_CORPUS=${CORPUS_ID}):"
  echo "    scripts/sovereign-ci-bench.sh"
else
  echo "⚠ index not present yet at $IDX — check 'sovereign corpus status' and the daemon log."
  echo "  If ingest stalled, confirm yield_to_foreground_secs < 30 and the daemon is healthy."
  exit 1
fi

#!/usr/bin/env bash
#
# setup-chaos-corpus.sh — one-command, reproducible setup of the chaos-monkey
# reference corpus (Conrad, *The Secret Agent*, Project Gutenberg #974).
#
# The chaos bench needs a SEALED, queryable corpus with known ground truth.
# `corpus watch` is the simplest ingest path, but it derives the corpus_id from
# the *path hash* (not --name), so we use a STABLE path (not /tmp) and print the
# resulting id to export as CHAOS_CORPUS for the CI bench.
#
# Prerequisites: a healthy daemon, and `yield_to_foreground_secs < 30` in
# ~/.sovereign/config.toml (otherwise the 30s health-ping starves the embed
# pipeline and ingest never completes — see chaos_monkey/README.md).
#
# Usage:  scripts/setup-chaos-corpus.sh [--dir <path>] [--bin <cli>]

set -euo pipefail

DIR="${HOME}/.sovereign/bench-corpora/chaos-secret-agent"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
URL="https://www.gutenberg.org/cache/epub/974/pg974.txt"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Warn if the daemon's yield window would starve ingest.
yld=$(grep -oE "yield_to_foreground_secs *= *[0-9]+" "$HOME/.sovereign/config.toml" 2>/dev/null | grep -oE "[0-9]+$" || echo "")
if [[ -n "$yld" ]] && (( yld >= 30 )); then
  echo "⚠ yield_to_foreground_secs=$yld ≥ 30 — the 30s health-ping will starve ingest."
  echo "  Set it below 30 (e.g. 15) in ~/.sovereign/config.toml and restart the daemon first."
fi

mkdir -p "$DIR"
if [[ ! -s "$DIR/secret-agent.txt" ]]; then
  echo "fetching The Secret Agent → $DIR/secret-agent.txt"
  curl -fsS --max-time 120 "$URL" -o "$DIR/secret-agent.txt"
fi
echo "corpus text: $(wc -l < "$DIR/secret-agent.txt") lines at $DIR/secret-agent.txt"

echo "watching $DIR as 'chaos-secret-agent' (waits for initial ingest)…"
"$BIN" corpus watch "$DIR" --name chaos-secret-agent --sync-initial

# Resolve the path-hash corpus_id the watch created (newest watched-* index
# whose meta points at $DIR is the one; fall back to the newest).
echo
echo "── next step ──"
echo "Find the corpus_id and point the CI bench at it:"
echo "    $BIN corpus list | grep -i secret    # or look for the newest watched-* id"
echo "    CHAOS_CORPUS=<that-id> scripts/sovereign-ci-bench.sh"
echo
echo "Or run the chaos bench directly:"
echo "    $BIN bench chaos-monkey run \\"
echo "      --bank sovereign/bench/chaos_monkey/secret_agent.toml \\"
echo "      --manifest sovereign/bench/chaos_monkey/manifest.toml \\"
echo "      --corpus <that-id>"
echo
echo "(For a machine-stable fixed corpus_id, install via a recipe instead of"
echo " corpus watch — tracked as a follow-up in chaos_monkey/README.md.)"

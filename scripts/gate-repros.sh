#!/usr/bin/env bash
# gate-repros — re-check the hazards behind embedded/gates.rs against
# real models. Run after a llama.cpp / llama-cpp-4 bump, or when a
# gate (e.g. prefix_cache_gate) is suspected of being over-applied.
#
#   ./scripts/gate-repros.sh --recurrent <path.gguf> \
#       [--fastshort <path.gguf>] [--attention <path.gguf>]
#
#   --recurrent  a recurrent/hybrid NON-MTP chat model (dense Qwen3.5,
#                qwen*moe, mamba…) — drives the prefix-cache hazard repro
#   --fastshort  a model fast_short_gate STILL vetoes after the
#                2026-06-11 narrowing (mamba/rwkv/deltanet/ssm arch) —
#                drives the FastShort hazard repro; defaults to --recurrent
#   --cleared    a model the narrowing CLEARED (qwen*moe like APEX, or
#                MTP-by-name) — drives the no-force burst canary that
#                guards the narrowing itself (recommended on every run)
#   --attention  a pure-attention model (qwen3, gemma, llama…) — drives
#                the harness-sanity control (recommended)
#
# Verdicts (see sovereign/crates/sovereign-inference/tests/gate_repros.rs):
#   test PASSES → hazard still reproduces → gate still justified
#   test FAILS with "HAZARD NO LONGER REPRODUCES" → upstream fixed it →
#     consider narrowing/removing that gate in src/embedded/gates.rs
set -euo pipefail

RECURRENT="" FASTSHORT="" CLEARED="" ATTENTION=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --recurrent) RECURRENT="$2"; shift 2 ;;
    --fastshort) FASTSHORT="$2"; shift 2 ;;
    --cleared)   CLEARED="$2"; shift 2 ;;
    --attention) ATTENTION="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown arg: $1 (try --help)" >&2; exit 2 ;;
  esac
done

if [[ -z "$RECURRENT" && -z "$FASTSHORT" && -z "$CLEARED" && -z "$ATTENTION" ]]; then
  echo "need at least one of --recurrent / --fastshort / --cleared / --attention (try --help)" >&2
  exit 2
fi
# Absolutize WITHOUT resolving symlinks — cargo test's CWD is the
# crate dir, not where we ran from, but the symlink NAME must survive:
# MTP detection keys on "mtp" in the model filename, and resolving a
# models/-dir symlink down to its HF-cache blob hash erases that
# signal (observed 2026-06-11: mtp_by_name=false on a *-MTP.gguf).
abspath() { case "$1" in /*) printf '%s\n' "$1";; *) printf '%s/%s\n' "$PWD" "$1";; esac; }
[[ -n "$RECURRENT" && ! -f "$RECURRENT" ]] && { echo "not a file: $RECURRENT" >&2; exit 2; }
[[ -n "$FASTSHORT" && ! -f "$FASTSHORT" ]] && { echo "not a file: $FASTSHORT" >&2; exit 2; }
[[ -n "$CLEARED" && ! -f "$CLEARED" ]] && { echo "not a file: $CLEARED" >&2; exit 2; }
[[ -n "$ATTENTION" && ! -f "$ATTENTION" ]] && { echo "not a file: $ATTENTION" >&2; exit 2; }
[[ -n "$RECURRENT" ]] && RECURRENT="$(abspath "$RECURRENT")"
[[ -n "$FASTSHORT" ]] && FASTSHORT="$(abspath "$FASTSHORT")"
[[ -n "$CLEARED" ]] && CLEARED="$(abspath "$CLEARED")"
[[ -n "$ATTENTION" ]] && ATTENTION="$(abspath "$ATTENTION")"

cd "$(dirname "$0")/.."

echo "── gate-repros: hazards re-checked against real weights ──────────"
echo "   recurrent: ${RECURRENT:-<unset — prefix-cache repro will SKIP>}"
echo "   fastshort: ${FASTSHORT:-<unset — falls back to --recurrent>}"
echo "   cleared:   ${CLEARED:-<unset — burst canary will SKIP>}"
echo "   attention: ${ATTENTION:-<unset — control will SKIP>}"
echo "   (a FAILING hazard repro is GOOD NEWS — read its message."
echo "    a FAILING canary/control is BAD news — read its message.)"
echo

# --test-threads=1 is load-bearing: the repros toggle process-global
# SOVEREIGN_* env flags around each scenario.
SOVEREIGN_REPRO_RECURRENT_GGUF="$RECURRENT" \
SOVEREIGN_REPRO_FASTSHORT_GGUF="$FASTSHORT" \
SOVEREIGN_REPRO_FASTSHORT_CLEARED_GGUF="$CLEARED" \
SOVEREIGN_REPRO_ATTENTION_GGUF="$ATTENTION" \
cargo test -p sovereign-inference --test gate_repros --release \
  -- --ignored --test-threads=1 --nocapture

#!/usr/bin/env bash
# Refactor anchor — emit a canonical fingerprint of enrichment + retrieval
# behaviour over the SEP corpus.
#
# The enrichment-as-plugin refactor moves ~1,900 lines across six crates and
# repoints ~90 files' imports. The workspace test suite is 11,872 tests and
# the repo's own record says a green suite of that size has failed to catch
# three breaks that running the real chain found. This is the real chain,
# reduced to something that runs in seconds.
#
# The fingerprint has two halves, and they fail for different reasons:
#
#   1. ATOMS  — sha256 of each anchor corpus's `atlas/atoms.json`. Catches a
#      change to what enrichment PRODUCES or how it is serialized. The
#      `sep-<slug>` index dirs are atlas-only sidecars (no `chunks.lance`),
#      so this is the only half that sees them at all.
#   2. RETRIEVAL — the ordered retrieved set, plus both scores, from
#      `svrn eval run` WITHOUT `--synth`. No model is in the scoring loop:
#      retrieval mode embeds the question, searches the index, and scores by
#      keyword. That determinism is the whole reason an anchor is viable.
#
# ARCH §18.4 (validate the instrument before the result): run this twice on
# unchanged code and require byte-identical output before trusting it.
# ARCH §18.1 (a gate you have not watched fail is not a gate): `--control`
# runs the deliberate-red case; the fingerprint MUST move.
#
# Usage:
#   capture.sh                 # emit the fingerprint on stdout
#   capture.sh --control       # positive control: --limit 5, fingerprint MUST differ
#   capture.sh --baseline      # write baseline.txt (both fingerprints + provenance)
#   capture.sh --verify        # compare against baseline.txt; exit 1 on drift
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../../../.." && pwd)"
bank="$here/bank.toml"
baseline="$here/baseline.txt"

# The three corpora the anchor question's `expected_sources` name. Each has an
# `atlas/atoms.json` installed locally; none has a `chunks.lance`.
ANCHOR_CORPORA=(sep-incompatibilism-arguments sep-compatibilism sep-freewill)
indexes="${SVRNMESH_DATA_DIR:-$HOME/.svrnmesh}/indexes"

# `sovereign` and `svrn` are the same binary under two names and not every
# host has both (AGENTS.md). Resolve whichever exists.
cli="${SOVEREIGN_CLI:-}"
if [[ -z "$cli" ]]; then
  for c in svrn sovereign; do
    if command -v "$c" >/dev/null 2>&1; then cli="$c"; break; fi
  done
fi
if [[ -z "$cli" ]]; then
  echo "capture.sh: neither \`svrn\` nor \`sovereign\` is on PATH" >&2
  exit 2
fi

mode="emit"
limit=10
case "${1:-}" in
  --control)  mode="emit";     limit=5 ;;
  --baseline) mode="baseline" ;;
  --verify)   mode="verify" ;;
  "")         ;;
  *) echo "capture.sh: unknown argument \`$1\`" >&2; exit 2 ;;
esac

# ── half 1: what enrichment produced ────────────────────────────────────────
atoms_lines() {
  local c f
  for c in "${ANCHOR_CORPORA[@]}"; do
    f="$indexes/$c/atlas/atoms.json"
    if [[ ! -f "$f" ]]; then
      # ARCH §18.3 — absence is reported, never defaulted. A missing sidecar
      # must not silently hash to nothing and read as "unchanged".
      echo "atoms  $c  MISSING"
      continue
    fi
    echo "atoms  $c  $(sha256sum "$f" | cut -d' ' -f1)"
  done
}

# ── half 2: what retrieval returned ─────────────────────────────────────────
# Identity + rounding rules are documented in fingerprint.py.
retrieval_lines() {
  "$cli" eval run --bank "$bank" --limit "$limit" --format json 2>/dev/null \
    | python3 "$here/fingerprint.py"
}

fingerprint() { atoms_lines; retrieval_lines; }

case "$mode" in
  emit)
    fingerprint
    ;;
  baseline)
    {
      echo "# refactor anchor baseline"
      echo "# repo    $repo_root"
      echo "# head    $(git -C "$repo_root" rev-parse --short HEAD)"
      echo "# written $(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "#"
      echo "# The DEFAULT block below is what every behaviour-preserving refactor"
      echo "# step must reproduce byte-for-byte. The CONTROL block is the same"
      echo "# capture with --limit 5: it exists so the next reader can see that"
      echo "# this anchor has been watched to move (ARCH §18.1). An anchor that"
      echo "# has only ever been green proves nothing."
      echo
      echo "=== DEFAULT (limit 10) ==="
      "$here/capture.sh"
      echo
      echo "=== CONTROL (limit 5) — MUST differ from DEFAULT ==="
      "$here/capture.sh" --control
    } > "$baseline"
    echo "wrote $baseline"
    ;;
  verify)
    if [[ ! -f "$baseline" ]]; then
      echo "capture.sh: no baseline at $baseline — run \`capture.sh --baseline\` first" >&2
      exit 2
    fi
    want="$(awk '/^=== DEFAULT/{f=1;next} /^=== CONTROL/{f=0} f' "$baseline" | sed '/^$/d')"
    got="$(fingerprint | sed '/^$/d')"
    if [[ "$want" == "$got" ]]; then
      echo "anchor: MATCH"
    else
      echo "anchor: DRIFT" >&2
      diff <(printf '%s\n' "$want") <(printf '%s\n' "$got") >&2 || true
      exit 1
    fi
    ;;
esac

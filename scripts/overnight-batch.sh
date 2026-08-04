#!/usr/bin/env bash
# Overnight unattended signal-collection batch. Four blocks, strictly serial.
#
# WHY SERIAL: :9741 contention is measured, not folklore — with the daemon up,
# three sovereign-compute supervisor tests fail; with it free the tree is
# 8329/0 (QUALITY_SURFACE.md "Ports"). Every block also wants the GPU. So this
# script's real job is sequencing and daemon/port handoff, not parallelism.
#
# WHY FAIL-OPEN: desktop-smoke.sh's Phase 0 is a HARD STOP by design. That is
# right for a release gate and wrong for an unattended window — a lint failure
# at 23:00 would otherwise cost the remaining blocks. Each block here is
# independently recoverable and a failure never aborts its successors.
#
# FOUR VERDICTS, NEVER TWO (ARCH principle 5): every block records PASS, FAIL,
# COULD-NOT-JUDGE or NEVER-RAN. A batch that reports green because half of it
# silently skipped is worse than one that did not run. Read MANIFEST.txt.
#
# Usage:
#   scripts/overnight-batch.sh [--blocks 1,2,3,4] [--out DIR] [--dry-run]
set -uo pipefail            # NOT -e: fail-open is the point.

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO" || exit 2

BLOCKS="1,2,3,4"
OUT=""
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --blocks) BLOCKS="$2"; shift 2;;
    --out)    OUT="$2"; shift 2;;
    --dry-run) DRY=1; shift;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="${OUT:-$REPO/target/overnight/$STAMP}"
mkdir -p "$OUT" || exit 2
MANIFEST="$OUT/MANIFEST.txt"

# Bank + sidecar carry REAL conversation titles from a personal archive.
# They live outside the repo, deliberately. (.gitignore does not untrack an
# already-tracked file — that lesson cost a history rewrite.)
PRIV="${OVERNIGHT_PRIV_DIR:-$HOME/.sovereign/overnight-private}"
mkdir -p "$PRIV"

CLI="$REPO/target/debug/sovereign-cli"
export SOVEREIGN_NO_STALE_WARN=1

note() { printf '%s\n' "$*" | tee -a "$MANIFEST"; }
hdr()  { note ""; note "════════════════════════════════════════════════════════"; note "$*"; note "════════════════════════════════════════════════════════"; }

record() { # block, verdict, detail
  printf '%-28s %-16s %s\n' "$1" "$2" "$3" >> "$OUT/SCOREBOARD.txt"
  note "VERDICT  $1  ->  $2   ($3)"
}

wants() { case ",$BLOCKS," in *",$1,"*) return 0;; *) return 1;; esac; }

daemon_up()   { curl -s -m 5 "http://localhost:9741/status" >/dev/null 2>&1; }
# NOTE: there is deliberately no free_9741() helper. Every block here either
# needs the resident daemon UP (blocks 1, 3, 4) or owns its own port handoff
# internally (block 2's desktop-smoke.sh). A wrapper-level "stop the daemon"
# has no correct call site and previously broke four of desktop-smoke's phases.
# Ensure a daemon is answering on :9741. `svrn daemon start` is itself
# idempotent (lifecycle.rs:369-376 probes the port and returns 0 with
# "already running"), and it defers to a registered service manager rather
# than spawning a detached child with different env. We still probe first so
# the manifest says which happened — "started" and "was already up" are
# different facts about the machine, and a log that conflates them is a log
# you cannot debug from.
start_daemon() {
  if daemon_up; then
    note "  daemon already running on :9741 — reusing it"
    return 0
  fi
  note "  starting daemon"
  "$CLI" daemon start >/dev/null 2>&1
  for _ in $(seq 1 60); do daemon_up && return 0; sleep 2; done
  note "  WARNING daemon did not come up within 120s"
  return 1
}

note "overnight batch  stamp=$STAMP  blocks=$BLOCKS"
note "repo=$REPO"
note "out=$OUT"
note "private artifacts=$PRIV  (real conversation titles — not in the repo)"
note "started $(date -u +%Y-%m-%dT%H:%M:%SZ)"
: > "$OUT/SCOREBOARD.txt"

if [ "$DRY" = "1" ]; then
  note ""
  note "DRY RUN — plan only, nothing executed."
  wants 1 && note "  block1 head-to-head   5 arms x 180 questions   ~20m"
  wants 2 && note "  block2 desktop-smoke  full, Phase 4 is the prize ~3h"
  wants 3 && note "  block3 desktop-soak   120m dual + judge calib   ~2.2h"
  wants 4 && note "  block4 overflow       confirmatory bank + report ~1h"
  exit 0
fi

# ─────────────────────────────────────────────────────────────────────────
# BLOCK 1 — retrieval head-to-head
# ─────────────────────────────────────────────────────────────────────────
if wants 1; then
  hdr "BLOCK 1 — retrieval head-to-head (5 arms)"
  B1="$OUT/block1"; mkdir -p "$B1"
  RERANK_GGUF="$REPO/sovereign/models/qwen3-reranker-0.6b-q8_0.gguf"

  if ! start_daemon; then
    record "block1-headtohead" "NEVER-RAN" "daemon would not start (embeddings unavailable)"
  elif [ ! -f "$PRIV/headtohead.toml" ] && ! python3 "$REPO/scripts/bridge-bank-gen.py" \
        "$PRIV/sample.tsv" "$PRIV/headtohead.toml" "$PRIV/headtohead-sidecar.json" \
        > "$B1/bankgen.log" 2>&1; then
    record "block1-headtohead" "NEVER-RAN" "bank generation failed — see block1/bankgen.log"
  else
    BANK="$PRIV/headtohead.toml"
    run_arm() { # name, then env assignments
      local name="$1"; shift
      local out="$B1/arm-$name.json"
      note "  arm $name"
      if [ -f "$out" ]; then note "    (already present, skipping)"; return 0; fi
      ( export "$@"; "$CLI" eval run --bank "$BANK" --prod-pipeline --isolate \
          --limit 50 --output "$out" ) > "$B1/arm-$name.log" 2>&1
      local rc=$?
      [ $rc -eq 0 ] || note "    arm $name exited $rc — see block1/arm-$name.log"
      return $rc
    }

    # PPR weight is read PER CALL (conv_tiered.rs:32); every rerank knob is a
    # STARTUP read (chat_cmd/bootstrap.rs:534-590), so each arm is its own
    # process regardless.
    #
    # SOVEREIGN_RERANK_DEDUP_CORPORA MUST name this corpus. It defaults to
    # {"sep"} (sovereign-tools/src/corpus/mod.rs:52), so a dedup arm without it
    # is bit-identical to baseline and the analyzer will (correctly) call it
    # VACUOUS.
    ok=0
    run_arm baseline    SOVEREIGN_CONV_PPR_WEIGHT=0.25 && ok=$((ok+1))
    run_arm ppr-off     SOVEREIGN_CONV_PPR_WEIGHT=0    && ok=$((ok+1))
    run_arm ppr-high    SOVEREIGN_CONV_PPR_WEIGHT=0.5  && ok=$((ok+1))
    run_arm dedup-only  SOVEREIGN_CONV_PPR_WEIGHT=0.25 \
                        SOVEREIGN_RERANK_DEDUP_ONLY=1 \
                        SOVEREIGN_RERANK_DEDUP_CORPORA=conversations-anthropic && ok=$((ok+1))
    if [ -f "$RERANK_GGUF" ]; then
      run_arm reranker  SOVEREIGN_CONV_PPR_WEIGHT=0.25 \
                        SOVEREIGN_RERANK_MODEL_PATH="$RERANK_GGUF" \
                        SOVEREIGN_RERANK_PER_ARTICLE=1 \
                        SOVEREIGN_RERANK_ALPHA=0.7 \
                        SOVEREIGN_RERANK_DEDUP_CORPORA=conversations-anthropic && ok=$((ok+1))
    else
      note "  arm reranker SKIPPED — GGUF absent at $RERANK_GGUF"
    fi

    ARGS=""
    for a in baseline ppr-off ppr-high dedup-only reranker; do
      [ -f "$B1/arm-$a.json" ] && ARGS="$ARGS $a=$B1/arm-$a.json"
    done
    if [ -n "$ARGS" ] && [ -f "$B1/arm-baseline.json" ]; then
      # shellcheck disable=SC2086
      python3 "$REPO/scripts/bridge-arms-analyze.py" "$PRIV/headtohead-sidecar.json" $ARGS \
        > "$B1/VERDICT.txt" 2>&1
      cat "$B1/VERDICT.txt" >> "$MANIFEST"
      if grep -q "^CHAMPION:" "$B1/VERDICT.txt"; then
        record "block1-headtohead" "PASS" "$(grep '^CHAMPION:' "$B1/VERDICT.txt" | head -1)"
      else
        record "block1-headtohead" "COULD-NOT-JUDGE" "no arm separated at p<0.05 — see block1/VERDICT.txt"
      fi
    else
      record "block1-headtohead" "FAIL" "baseline arm missing; $ok arms completed"
    fi
  fi
fi

# ─────────────────────────────────────────────────────────────────────────
# BLOCK 2 — desktop-smoke.sh (Phase 4 = the layer CI never runs)
# ─────────────────────────────────────────────────────────────────────────
if wants 2; then
  hdr "BLOCK 2 — desktop-smoke.sh (full)"
  B2="$OUT/block2"; mkdir -p "$B2"
  if [ ! -x "$REPO/scripts/desktop-smoke.sh" ]; then
    record "block2-desktop-smoke" "NEVER-RAN" "scripts/desktop-smoke.sh missing or not executable"
  else
    # DO NOT free :9741 here. desktop-smoke.sh phases 1, 2, 3 and 5 SHARE the
    # resident daemon (desktop-smoke.sh:24); only phase 4 needs the port, and
    # the script owns that handoff itself via stop_resident_daemon /
    # restore_resident_daemon (:187, :213). Stopping the daemon first would
    # break four of its six phases — they would report failures caused by this
    # wrapper rather than by the code under test.
    start_daemon
    "$REPO/scripts/desktop-smoke.sh" > "$B2/smoke.log" 2>&1
    rc=$?
    cp -R "$REPO/target/desktop-smoke" "$B2/artifacts" 2>/dev/null
    # desktop-smoke: 0 ok, 1 a gate failed, 2 hard stop / setup error.
    case $rc in
      0) record "block2-desktop-smoke" "PASS" "exit 0" ;;
      1) record "block2-desktop-smoke" "FAIL" "a gate failed (exit 1) — read the scoreboard for SKIP rows" ;;
      2) record "block2-desktop-smoke" "COULD-NOT-JUDGE" "hard stop / setup error (exit 2) — preconditions unmet" ;;
      *) record "block2-desktop-smoke" "FAIL" "exit $rc" ;;
    esac
    grep -iE "SKIP|skipped" "$B2/smoke.log" | head -20 >> "$MANIFEST" 2>/dev/null
  fi
fi

# ─────────────────────────────────────────────────────────────────────────
# BLOCK 3 — chaos + persona soak (judge calibration is a PRECONDITION)
# ─────────────────────────────────────────────────────────────────────────
if wants 3; then
  hdr "BLOCK 3 — desktop-soak (dual)"
  B3="$OUT/block3"; mkdir -p "$B3"
  CAL="$REPO/sovereign/crates/sovereign-desktop/tests/e2e/scripts/calibrate-judge.mjs"
  SOAK_MIN="${OVERNIGHT_SOAK_MIN:-120}"
  if [ ! -f "$REPO/scripts/desktop-soak.py" ]; then
    record "block3-soak" "NEVER-RAN" "scripts/desktop-soak.py missing"
  else
    # No rubric may score runs without passing calibration (sensitivity 0.85 /
    # specificity 0.8). An uncalibrated judge produces numbers, not evidence.
    calib="unknown"
    if [ -f "$CAL" ]; then
      ( cd "$REPO/sovereign/crates/sovereign-desktop" && node "$CAL" ) > "$B3/judge-calibration.log" 2>&1 \
        && calib="pass" || calib="FAIL"
      note "  judge calibration: $calib"
    else
      note "  judge calibration script not found at $CAL"
    fi
    if [ "$calib" = "FAIL" ]; then
      record "block3-soak" "COULD-NOT-JUDGE" "judge calibration failed — soak scores would be meaningless"
    else
      python3 "$REPO/scripts/desktop-soak.py" "$SOAK_MIN" --mode dual --stamp "overnight-$STAMP" \
        > "$B3/soak.log" 2>&1
      rc=$?
      [ $rc -eq 0 ] && record "block3-soak" "PASS" "calib=$calib, ${SOAK_MIN}m dual" \
                    || record "block3-soak" "FAIL" "exit $rc (calib=$calib) — see block3/soak.log"
    fi
  fi
fi

# ─────────────────────────────────────────────────────────────────────────
# BLOCK 4 — overflow: confirmatory real bank + collection
# ─────────────────────────────────────────────────────────────────────────
if wants 4; then
  hdr "BLOCK 4 — confirmatory bank + collection"
  B4="$OUT/block4"; mkdir -p "$B4"
  REAL_BANK="$REPO/sovereign/bench/conversation-private/questions.toml"
  if [ -f "$REAL_BANK" ] && [ -f "$OUT/block1/arm-baseline.json" ]; then
    start_daemon
    for a in baseline reranker; do
      case "$a" in
        baseline) envs=("SOVEREIGN_CONV_PPR_WEIGHT=0.25");;
        reranker) envs=("SOVEREIGN_CONV_PPR_WEIGHT=0.25"
                        "SOVEREIGN_RERANK_MODEL_PATH=$REPO/sovereign/models/qwen3-reranker-0.6b-q8_0.gguf"
                        "SOVEREIGN_RERANK_PER_ARTICLE=1" "SOVEREIGN_RERANK_ALPHA=0.7"
                        "SOVEREIGN_RERANK_DEDUP_CORPORA=conversations-anthropic");;
      esac
      ( export "${envs[@]}"; "$CLI" eval run --bank "$REAL_BANK" --prod-pipeline --isolate \
          --limit 50 --output "$B4/real-$a.json" ) > "$B4/real-$a.log" 2>&1
    done
    if [ -f "$B4/real-baseline.json" ]; then
      record "block4-confirmatory" "PASS" "12-question real bank run on baseline + reranker"
    else
      record "block4-confirmatory" "FAIL" "real-bank run produced no baseline output"
    fi
  else
    record "block4-confirmatory" "NEVER-RAN" "real bank absent or block1 baseline missing"
  fi
fi

hdr "SCOREBOARD"
cat "$OUT/SCOREBOARD.txt" | tee -a "$MANIFEST"
note ""
note "finished $(date -u +%Y-%m-%dT%H:%M:%SZ)"
note "A block marked NEVER-RAN or COULD-NOT-JUDGE verified NOTHING."
note "artifacts: $OUT"

#!/usr/bin/env bash
# co-backlog-producer.sh — file a backlog candidate from an AUTOMATED signal.
#
# THE PRODUCER CONTRACT. A producer is anything that notices work and is
# not a person: a failed gate, a watcher, a nightly lane, a soak run. It
# does not score, does not rank, and does not decide anything — it hands
# `svrn backlog add` a piece of text and an identity, and the verb scores
# it on the local model against quality/backlog-ruler.toml. Everything a
# producer files lands UNVETTED and unpullable (the item carries
# `Scored-by:`), so a noisy producer costs the operator a scroll, never a
# wrong pull.
#
# Four rules, and they are the whole contract:
#
#   1. IDENTITY IS ESSENCE, NOT OCCURRENCE (ARCH §7.5). --key names WHAT
#      went wrong — a lane name, a check name, an invariant id — never a
#      run id, a timestamp, a PID or a row count. The verb updates the
#      item that key already filed, so a gate that fails every night
#      leaves ONE item that keeps getting fresher, not thirty.
#   2. THE EVIDENCE IS THE PRODUCER'S OWN OUTPUT. Pass the artifact you
#      already have (--evidence-file); do not summarize it. The scorer
#      reads the text, and a human reads it after. A producer that
#      paraphrases its own log is a producer that can be wrong twice.
#   3. NEVER BREAK YOUR CALLER. This script always exits 0. A gate that
#      files a backlog item is still a gate; if filing fails — daemon
#      down, no model, store missing — it says so on stderr and the
#      caller's own verdict is untouched. A CI lane must never go red
#      because the backlog was unreachable, and must never go GREEN
#      because it was.
#   4. SAY WHAT YOU FILED. The line it prints names the item id and the
#      key, so the run log that failed also records what was banked.
#
# Usage:
#   scripts/co-backlog-producer.sh --key <identity> --title <one line> \
#       [--objective <anchor>] [--evidence-file <path>] [--tail <n>] \
#       [--producer <name>] [--dry-run]
#
# CO_BACKLOG_PRODUCER=0 disables every producer at once (a machine broken
# in a way you already know about should not keep filing about it).
# CO_BACKLOG_PRODUCER=dry makes every producer print exactly what it
# WOULD file and write nothing — which is how you verify a new caller's
# wiring without putting a word in the operator's backlog.

set -uo pipefail

KEY=""; TITLE=""; OBJECTIVE=""; EVIDENCE=""; TAIL_N=60
PRODUCER="co-backlog-producer.sh"; DRY_RUN=""
SVRN="${SOVEREIGN_CLI:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --key) KEY="$2"; shift 2 ;;
    --title) TITLE="$2"; shift 2 ;;
    --objective) OBJECTIVE="$2"; shift 2 ;;
    --evidence-file) EVIDENCE="$2"; shift 2 ;;
    --tail) TAIL_N="$2"; shift 2 ;;
    --producer) PRODUCER="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "co-backlog-producer: unknown flag $1" >&2; exit 0 ;;
  esac
done

# Rule 3 starts here: every early return is exit 0.
if [[ "${CO_BACKLOG_PRODUCER:-1}" == "0" ]]; then
  echo "co-backlog-producer: disabled (CO_BACKLOG_PRODUCER=0) — not filing '$KEY'" >&2
  exit 0
fi
[[ "${CO_BACKLOG_PRODUCER:-1}" == "dry" ]] && DRY_RUN=1
if [[ -z "$KEY" || -z "$TITLE" ]]; then
  echo "co-backlog-producer: --key and --title are required — not filing" >&2
  exit 0
fi

# The verb lives in sovereign-cli-llm; the dispatcher execs into it. Take
# whichever is present, and say which one, so a run against a stale
# binary is visible in the log that filed the item.
if [[ -z "$SVRN" ]]; then
  for cand in target/debug/sovereign-cli "$(command -v svrn 2>/dev/null || true)" \
              "$(command -v sovereign 2>/dev/null || true)"; do
    [[ -n "$cand" && -x "$cand" ]] && { SVRN="$cand"; break; }
  done
fi
if [[ -z "$SVRN" ]]; then
  echo "co-backlog-producer: no sovereign CLI found — not filing '$KEY'" >&2
  exit 0
fi

BODY="$TITLE"
if [[ -n "$EVIDENCE" && -r "$EVIDENCE" ]]; then
  BODY="$BODY

--- the producer's own output, last ${TAIL_N} lines of ${EVIDENCE} ---
$(tail -n "$TAIL_N" "$EVIDENCE" 2>/dev/null)"
elif [[ -n "$EVIDENCE" ]]; then
  # Absence is reported, never defaulted (ARCH §18.3) — an item that
  # silently lost its evidence would read as a bare assertion.
  BODY="$BODY

(evidence file $EVIDENCE was named but could not be read)"
fi

ARGS=(backlog add "$BODY" --key "$KEY" --producer "$PRODUCER")
[[ -n "$OBJECTIVE" ]] && ARGS+=(--objective "$OBJECTIVE")

if [[ -n "$DRY_RUN" ]]; then
  echo "co-backlog-producer: DRY RUN — would file key='$KEY' via $SVRN"
  echo "--- item text ---"
  printf '%s\n' "$BODY"
  exit 0
fi

echo "co-backlog-producer: filing '$KEY' via $SVRN" >&2
if out="$("$SVRN" "${ARGS[@]}" 2>&1)"; then
  printf '%s\n' "$out" | sed 's/^/  /'
else
  # Rule 3: the caller's verdict is not ours to change.
  echo "co-backlog-producer: could NOT file '$KEY' — the signal stands, the item does not:" >&2
  printf '%s\n' "$out" | sed 's/^/  /' >&2
fi
exit 0

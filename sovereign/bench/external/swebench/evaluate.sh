#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Grade one arm's predictions with the OFFICIAL SWE-bench harness.
#
# Nothing in this repo decides resolved/unresolved. That is the whole
# point of the external ruler: the arms produce diffs, and someone
# else's code says whether they work.
#
#   ./evaluate.sh native
#   ./evaluate.sh comaintainer --workers 4 --engine docker
#
# CONTAINER ENGINE. Prefers podman (operator preference, 2026-08-18).
# The SWE-bench harness talks the Docker API via docker-py, so podman
# is used through its API socket: the machine is started if needed and
# DOCKER_HOST is pointed at it. No docker daemon is required.
#
# ARM64. The prebuilt `swebench/*` images are x86_64. On Apple silicon
# this drops --namespace so the harness builds images locally (slow on
# first run, cached after) — and a minority of instances have
# environments that do not build on arm64 at all. The honest options
# are (a) run this leg on the x86 Fedora peer, or (b) accept a smaller
# denominator and SAY SO. It is not honest to let arm64 build failures
# land in the unresolved column.
set -euo pipefail

ARM="${1:?usage: evaluate.sh <arm> [--workers N] [--run-id ID] [--engine podman|docker]}"
shift || true

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREDS="$HERE/predictions/$ARM.jsonl"
WORKERS=4
RUN_ID="$ARM-$(date -u +%Y%m%dT%H%M%SZ)"
ENGINE=auto

while [[ $# -gt 0 ]]; do
  case "$1" in
    --workers) WORKERS="$2"; shift 2 ;;
    --run-id)  RUN_ID="$2";  shift 2 ;;
    --engine)  ENGINE="$2";  shift 2 ;;
    *) echo "unknown flag $1" >&2; exit 2 ;;
  esac
done

[[ -f "$PREDS" ]] || { echo "no predictions at $PREDS — run collect.py --arm $ARM" >&2; exit 1; }

# ── container engine ────────────────────────────────────────────────
if [[ "$ENGINE" == "auto" ]]; then
  if command -v podman >/dev/null 2>&1; then ENGINE=podman; else ENGINE=docker; fi
fi

case "$ENGINE" in
  podman)
    command -v podman >/dev/null 2>&1 || { echo "podman not on PATH" >&2; exit 1; }
    # macOS/Windows run podman in a VM; Linux talks to the socket directly.
    if podman machine list --format '{{.Name}}' 2>/dev/null | grep -q .; then
      state="$(podman machine inspect --format '{{.State}}' 2>/dev/null | head -1)"
      if [[ "$state" != "running" ]]; then
        echo "starting podman machine (state=$state) …" >&2
        podman machine start
      fi
      SOCK="$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}' 2>/dev/null | head -1)"
    else
      SOCK="$(podman info --format '{{.Host.RemoteSocket.Path}}' 2>/dev/null)"
    fi
    [[ -n "${SOCK:-}" ]] || { echo "could not resolve podman socket" >&2; exit 1; }
    export DOCKER_HOST="unix://$SOCK"
    echo "engine: podman via $DOCKER_HOST" >&2
    ;;
  docker)
    docker info >/dev/null 2>&1 || { echo "docker daemon not reachable" >&2; exit 1; }
    echo "engine: docker" >&2
    ;;
  *) echo "unknown engine $ENGINE" >&2; exit 2 ;;
esac

NAMESPACE_ARG=(--namespace swebench)
case "$(uname -m)" in
  arm64|aarch64)
    echo "arm64 host: building images locally (no prebuilt namespace)" >&2
    NAMESPACE_ARG=(--namespace '')
    ;;
esac

echo "grading arm=$ARM  run_id=$RUN_ID  workers=$WORKERS"
uvx --from swebench python -m swebench.harness.run_evaluation \
  --dataset_name princeton-nlp/SWE-bench_Verified \
  --predictions_path "$PREDS" \
  --run_id "$RUN_ID" \
  --max_workers "$WORKERS" \
  "${NAMESPACE_ARG[@]}"

echo
echo "report written to ./*.$RUN_ID.json (harness writes to CWD)"

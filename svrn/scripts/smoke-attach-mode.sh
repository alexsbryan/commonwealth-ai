#!/usr/bin/env bash
# Attach-mode smoke test. Verifies the full chain wired up in the
# "Desktop attaches to CLI-started daemon + shared config" work:
#   1. `sovereign daemon run` is live on :9741.
#   2. /v1/models answers — bootstrap probe would return Attach.
#   3. /v1/mesh/status answers — mesh mutation HTTP surface is merged.
#   4. /v1/admin/reload answers 200 on a no-op (nothing changed on
#      disk) — admin surface is merged and config baseline is set.
#
# Run after `sovereign setup --yes` and before `cargo tauri dev`.
# Exits non-zero on the first failing check.
set -euo pipefail

PORT="${SOVEREIGN_CLIENT_PORT:-9741}"
BASE="http://127.0.0.1:${PORT}"

echo "▶ probing sovereign daemon at ${BASE}"

check() {
  local path="$1" label="$2" method="${3:-GET}"
  local code
  if [[ "$method" == "POST" ]]; then
    code=$(curl -s -o /dev/null -w "%{http_code}" \
      -X POST -H 'content-type: application/json' -d '{}' \
      --max-time 5 "${BASE}${path}")
  else
    code=$(curl -s -o /dev/null -w "%{http_code}" \
      --max-time 5 "${BASE}${path}")
  fi
  if [[ "$code" =~ ^2 ]]; then
    echo "  ✓ ${label} (${method} ${path}) → ${code}"
  else
    echo "  ✗ ${label} (${method} ${path}) → ${code}"
    exit 1
  fi
}

check /v1/models         "OpenAI models list"
check /v1/mesh/status    "mesh status"
check /v1/admin/reload   "admin reload (no-op)" POST

echo "✓ attach-mode surface is live and complete"

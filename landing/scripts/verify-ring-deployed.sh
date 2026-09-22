#!/usr/bin/env bash
# Verify the guest runtime actually shipped with the deploy.
#
# The one failure mode a build-and-deploy script cannot see: the runtime is
# built, but the CLI did not upload it (an ignore-file precedence surprise), so
# svrnme.sh/ring/ 404s and a guest's QR opens nothing. This fetches it and
# fails loudly if it is absent — a deploy that did not ship what it built is
# not a successful deploy.
#
#   RING_ORIGIN=https://svrnme.sh scripts/verify-ring-deployed.sh
set -euo pipefail
ORIGIN="${RING_ORIGIN:-https://svrnme.sh}"
URL="${ORIGIN%/}/ring/index.html"

for i in 1 2 3 4 5 6; do
  code="$(curl -fsS -o /dev/null -w '%{http_code}' "$URL" 2>/dev/null || true)"
  if [ "$code" = "200" ]; then
    echo "verify-ring: $URL is live (200)"
    exit 0
  fi
  sleep 5
done
echo "verify-ring: $URL did not return 200 after 30s — the deploy did not ship" >&2
echo "  the runtime. Check landing/.vercelignore (ring/ must not be excluded)" >&2
echo "  and that 'npm run build:ring' wrote landing/ring/ before the deploy." >&2
exit 1

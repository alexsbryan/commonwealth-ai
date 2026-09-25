#!/usr/bin/env bash
# Verify the guest runtime actually shipped with the deploy — and that the
# page a phone opens can LOAD its runtime.
#
# Two failure modes this catches:
#
#   1. The runtime is built but not uploaded (an ignore-file precedence
#      surprise), so /ring/ 404s and a guest's QR opens nothing.
#
#   2. The runtime is uploaded and /ring/ 200s, but the host's trailing-slash
#      normalization strips the slash from the page URL — then the page's
#      relative `./app.js` resolves to /app.js, 404s, and the page sits on its
#      static "Connecting…" forever. Exactly this shipped to svrnme.sh on
#      2026-09-22 (trailingSlash: false): a status-only check reported "live"
#      while every wall link was dead. So this script resolves the page's
#      script reference the way a browser does — against the URL the server
#      actually serves the page at — and fetches THAT.
#
#   RING_ORIGIN=https://svrnme.sh scripts/verify-ring-deployed.sh
set -euo pipefail
ORIGIN="${RING_ORIGIN:-https://svrnme.sh}"
URL="${ORIGIN%/}/ring/index.html"
PAGE="$(mktemp -t ring-page.XXXXXX)"
trap 'rm -f "$PAGE"' EXIT

check() {
  # (a) the page is reachable and IS the guest page
  local effective
  effective="$(curl -fsSL -o "$PAGE" -w '%{url_effective}' "$URL" 2>/dev/null || true)"
  [ -n "$effective" ] || { echo "verify-ring: $URL did not answer"; return 1; }
  grep -q "Ring guest" "$PAGE" || { echo "verify-ring: $effective answered but is not the guest page"; return 1; }

  # (b)+(c) resolve the script reference the way a browser will, and fetch it.
  #
  # Two forms are correct and both are checked, because the page may either
  # keep its shell RELATIVE (which requires the URL to keep its trailing
  # slash) or name it ABSOLUTE. The 2026-09-22 outage was the first form with
  # the slash stripped; an absolute shell survives that same strip, so failing
  # it on the slash alone would be a false negative. What must never pass is a
  # page whose runtime does not fetch.
  local ref asset code relative=0
  ref="$(grep -oE 'src="\./[^"]+"' "$PAGE" | head -1 | sed 's/^src="//; s/"$//')"
  if [ -n "$ref" ]; then
    relative=1
  else
    ref="$(grep -oE 'src="/[^"]+"' "$PAGE" | head -1 | sed 's/^src="//; s/"$//')"
  fi
  [ -n "$ref" ] || { echo "verify-ring: $effective has no script reference"; return 1; }

  if [ "$relative" = 1 ]; then
    case "$effective" in
      */) ;;
      *) echo "verify-ring: the served page URL lost its trailing slash: $effective"
         echo "  A browser resolves './app.js' against the last path segment, so the"
         echo "  runtime would be fetched from ${effective%/ring*}/app.js — a 404."
         echo "  Check landing/vercel.json: no global trailingSlash:false; /ring/ must"
         echo "  stay /ring/ (the /ring → /ring/ redirect is the canonical form)."
         return 1 ;;
    esac
    asset="$effective${ref#./}"
  else
    # Against the origin the page ACTUALLY served from, not the origin we
    # asked. The apex 308s to www, and a browser resolves an absolute ref
    # against the final URL — following the redirect here is what makes this
    # check the browser's question rather than a nearby one.
    local base
    base="$(printf '%s' "$effective" | sed -E 's#^(https?://[^/]+).*#\1#')"
    asset="$base$ref"
  fi
  code="$(curl -fsS -o /dev/null -w '%{http_code}' "$asset" 2>/dev/null || true)"
  if [ "$code" != "200" ]; then
    echo "verify-ring: the page loads, but its runtime does not: $asset → ${code:-no answer}"
    echo "  This is the 'Connecting…' failure mode; the wall QR opens a dead page."
    return 1
  fi

  local form="absolute"
  [ "$relative" = 1 ] && form="relative"
  echo "verify-ring: $effective is live, shell $form, runtime at $asset (200)"
  return 0
}

for i in 1 2 3 4 5 6; do
  if check; then
    exit 0
  fi
  [ "$i" = "6" ] || sleep 5
done
echo "verify-ring: still failing after 30s — the deploy did not ship a working runtime." >&2
echo "  Also check landing/.vercelignore (ring/ must not be excluded) and that" >&2
echo "  'npm run build:ring' wrote landing/ring/ before the deploy." >&2
exit 1

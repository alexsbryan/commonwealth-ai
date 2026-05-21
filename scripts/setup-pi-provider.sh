#!/usr/bin/env bash
# Idempotent: write a `commonwealth` provider into ~/.pi/agent/models.json
# pointing at the local daemon's OpenAI-compatible /v1 surface.
#
# Pi's custom-provider docs (providers.md / models.md) describe the
# `models.json` shape. We use the `openai-completions` API surface.
# Run safely repeatedly — if a `commonwealth` provider is already
# present we leave it alone unless `--force` is passed.

set -euo pipefail

FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--force]

Writes ~/.pi/agent/models.json with a 'commonwealth' provider
that points at http://localhost:9741/v1 with model
'commonwealth/coder'. Skips if a 'commonwealth' provider already
exists, unless --force is given.
EOF
            exit 0
            ;;
        *)
            echo "unknown flag: $arg" >&2
            exit 2
            ;;
    esac
done

CONFIG_DIR="$HOME/.pi/agent"
CONFIG_FILE="$CONFIG_DIR/models.json"
mkdir -p "$CONFIG_DIR"

# Default content — written when the file is absent or `--force`.
read -r -d '' DEFAULT_BODY <<'JSON' || true
{
  "providers": {
    "commonwealth": {
      "baseUrl": "http://localhost:9741/v1",
      "api": "openai-completions",
      "apiKey": "dummy",
      "models": [
        {
          "id": "commonwealth/coder",
          "name": "Commonwealth Coder",
          "contextWindow": 32768
        },
        {
          "id": "commonwealth/primary",
          "name": "Commonwealth Primary",
          "contextWindow": 32768
        }
      ]
    }
  }
}
JSON

if [[ ! -f "$CONFIG_FILE" ]]; then
    echo "writing $CONFIG_FILE (no prior config)"
    printf '%s\n' "$DEFAULT_BODY" > "$CONFIG_FILE"
    exit 0
fi

if [[ "$FORCE" == "1" ]]; then
    echo "writing $CONFIG_FILE (--force)"
    cp -f "$CONFIG_FILE" "$CONFIG_FILE.bak"
    printf '%s\n' "$DEFAULT_BODY" > "$CONFIG_FILE"
    exit 0
fi

if grep -q '"commonwealth"' "$CONFIG_FILE"; then
    echo "$CONFIG_FILE already has a 'commonwealth' provider; not modifying."
    echo "(use --force to overwrite)"
    exit 0
fi

# File exists but no `commonwealth` provider — fail loudly rather than
# trying to merge JSON in bash. Operator can either delete the file or
# add the provider manually.
echo "error: $CONFIG_FILE exists but has no 'commonwealth' provider." >&2
echo "       Add one manually (see ~/.nvm/.../pi-coding-agent/docs/models.md)" >&2
echo "       or rerun with --force to overwrite the file." >&2
exit 1

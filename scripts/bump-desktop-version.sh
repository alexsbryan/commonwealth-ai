#!/usr/bin/env bash
# bump-desktop-version.sh — bump the Sovereign desktop app's version across
# all three files that have to move together, then verify the trio agrees.
#
# Usage:
#   scripts/bump-desktop-version.sh 0.2.0          # set to an explicit version
#   scripts/bump-desktop-version.sh patch          # 0.1.0 -> 0.1.1
#   scripts/bump-desktop-version.sh minor          # 0.1.0 -> 0.2.0
#   scripts/bump-desktop-version.sh major          # 0.1.0 -> 1.0.0
#
# What it does (no git operations):
#   1. Reads the current version from the workspace root Cargo.toml.
#   2. Computes the new version (explicit arg OR bump from current).
#   3. Writes the new version to the three files that must agree:
#        - Cargo.toml                                      (workspace.package.version)
#        - sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json   (version)
#        - sovereign/crates/sovereign-desktop/package.json                (version)
#   4. Runs check-desktop-version.sh to confirm all three now match.
#   5. Prints the suggested commit/tag/push lines. Does NOT run them.
#
# Why no auto-commit? Releasing is a deliberate act. We finalize the files;
# the human reviews the diff and runs `git tag` when they're ready. The
# CI workflow keys off `git push origin desktop-v<X.Y.Z>`, so the tag is
# what kicks off the release.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

CARGO_TOML="$REPO_ROOT/Cargo.toml"
TAURI_CONF="$REPO_ROOT/sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json"
PACKAGE_JSON="$REPO_ROOT/sovereign/crates/sovereign-desktop/package.json"

if [[ $# -ne 1 ]]; then
    cat <<EOF >&2
Usage: scripts/bump-desktop-version.sh <version | major | minor | patch>

Examples:
  scripts/bump-desktop-version.sh 0.2.0           # set explicit
  scripts/bump-desktop-version.sh patch           # 0.1.0 -> 0.1.1
  scripts/bump-desktop-version.sh minor           # 0.1.0 -> 0.2.0
  scripts/bump-desktop-version.sh major           # 0.1.0 -> 1.0.0
EOF
    exit 2
fi

arg="$1"

for f in "$CARGO_TOML" "$TAURI_CONF" "$PACKAGE_JSON"; do
    if [[ ! -f "$f" ]]; then
        echo "bump-desktop-version: missing file: $f" >&2
        exit 2
    fi
done

# ── Read current version from Cargo.toml's [workspace.package] section ──
# Section-aware so we don't accidentally pick up any [package] version
# that might land elsewhere in the file later.
CURRENT_VERSION="$(awk '
    /^\[workspace\.package\][[:space:]]*$/ { in_section = 1; next }
    /^\[/                                  { in_section = 0 }
    in_section && /^version[[:space:]]*=[[:space:]]*"/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*/, "")
        print
        exit
    }
' "$CARGO_TOML")"

if [[ -z "$CURRENT_VERSION" ]]; then
    echo "bump-desktop-version: could not read workspace.package.version from $CARGO_TOML" >&2
    exit 1
fi

# ── Resolve target version ──
if [[ "$arg" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]]; then
    NEW_VERSION="$arg"
elif [[ "$arg" == "major" || "$arg" == "minor" || "$arg" == "patch" ]]; then
    # Strip any pre-release suffix before bumping numeric components.
    base="${CURRENT_VERSION%%-*}"
    IFS='.' read -r maj min pat <<< "$base"
    case "$arg" in
        major) maj=$((maj + 1)); min=0; pat=0 ;;
        minor) min=$((min + 1)); pat=0 ;;
        patch) pat=$((pat + 1)) ;;
    esac
    NEW_VERSION="$maj.$min.$pat"
else
    echo "bump-desktop-version: '$arg' is not a valid semver or one of {major,minor,patch}" >&2
    exit 1
fi

if [[ "$NEW_VERSION" == "$CURRENT_VERSION" ]]; then
    echo "Version is already $NEW_VERSION; nothing to do."
    exit 0
fi

echo "Bumping desktop version: $CURRENT_VERSION -> $NEW_VERSION"
echo

# ── Update Cargo.toml (section-aware) ──
python3 - "$CARGO_TOML" "$NEW_VERSION" <<'PYEOF'
import re, sys
path, new_version = sys.argv[1], sys.argv[2]
with open(path) as f:
    lines = f.readlines()
in_section = False
done = False
for i, line in enumerate(lines):
    stripped = line.strip()
    if stripped == "[workspace.package]":
        in_section = True
        continue
    if stripped.startswith("[") and stripped.endswith("]"):
        in_section = False
        continue
    if in_section and re.match(r'^version\s*=\s*"', line):
        # Preserve any trailing comment after the value.
        lines[i] = re.sub(
            r'^(version\s*=\s*)"[^"]*"',
            rf'\1"{new_version}"',
            line, count=1,
        )
        done = True
        break
if not done:
    sys.stderr.write(f"could not find workspace.package.version in {path}\n")
    sys.exit(1)
with open(path, 'w') as f:
    f.writelines(lines)
PYEOF
echo "  updated  $CARGO_TOML"

# ── Update JSON files in-place (regex, not json.dump, to preserve diff cleanliness) ──
# `count=1` ensures we only touch the first top-level "version" key — both
# files have it at the top of the document before any nested objects.
update_json_version() {
    local path="$1" new_version="$2"
    python3 - "$path" "$new_version" <<'PYEOF'
import json, re, sys
path, new_version = sys.argv[1], sys.argv[2]
# Verify the file parses as JSON and has a "version" key before touching it,
# so a malformed file fails loudly instead of silently no-op'ing.
with open(path) as f:
    raw = f.read()
parsed = json.loads(raw)
if "version" not in parsed:
    sys.stderr.write(f'no top-level "version" key in {path}\n')
    sys.exit(1)
new_raw = re.sub(
    r'^(\s*)"version"(\s*):(\s*)"[^"]+"',
    rf'\1"version"\2:\3"{new_version}"',
    raw, count=1, flags=re.MULTILINE,
)
if new_raw == raw:
    sys.stderr.write(f'failed to substitute version in {path}\n')
    sys.exit(1)
with open(path, 'w') as f:
    f.write(new_raw)
PYEOF
}

update_json_version "$TAURI_CONF"   "$NEW_VERSION"
echo "  updated  $TAURI_CONF"
update_json_version "$PACKAGE_JSON" "$NEW_VERSION"
echo "  updated  $PACKAGE_JSON"

echo

# ── Verify with the existing consistency checker ──
"$SCRIPT_DIR/check-desktop-version.sh" "$NEW_VERSION"

# ── Suggested follow-up ──
cat <<EOF

Next:
  git diff                                                              # review
  git add Cargo.toml \\
          sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json \\
          sovereign/crates/sovereign-desktop/package.json
  git commit -m 'chore(desktop): release v$NEW_VERSION'
  git tag desktop-v$NEW_VERSION
  git push origin main desktop-v$NEW_VERSION                            # kicks CI

EOF

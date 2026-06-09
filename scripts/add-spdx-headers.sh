#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Idempotent SPDX header sweep. Prepends the AGPL-3.0-or-later SPDX tag to
# every first-party source file (.rs/.ts/.js/.mjs/.svelte) that lacks one.
# Re-runnable: files already carrying a tag (in the first few lines) are
# skipped. Run again after adding new files, and before a publish squash.
#
#   DRY_RUN=1 scripts/add-spdx-headers.sh   # list what would change
#   scripts/add-spdx-headers.sh             # apply
#
# Excludes build output (target/, dist/, build/), dependencies
# (node_modules/), and vendored third-party code (vendor/) — we only tag
# code we're licensing.
set -eu

TAG="SPDX-License-Identifier: AGPL-3.0-or-later"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
DRY_RUN="${DRY_RUN:-0}"

added=0
skipped=0

# $1=file  $2=comment-prefix  $3=comment-suffix
process() {
  f="$1"; prefix="$2"; suffix="$3"
  # Idempotent: skip if a tag already sits near the top. Checked against
  # the first 5 lines only, so a file that merely *mentions* the tag in its
  # body isn't mistaken for an already-headed file.
  if head -5 "$f" | grep -q 'SPDX-License-Identifier'; then
    skipped=$((skipped + 1))
    return
  fi
  line="${prefix}${TAG}${suffix}"
  if [ "$DRY_RUN" = "1" ]; then
    echo "  + $f"
    added=$((added + 1))
    return
  fi
  first="$(head -1 "$f")"
  # Preserve a real shebang (`#!/...` or `#! ...`) by inserting the tag on
  # line 2. A Rust inner attribute (`#![...]`) is NOT a shebang — the tag
  # goes on line 1, before it (a comment may precede a crate attribute).
  case "$first" in
    '#!/'* | '#! '*)
      { head -1 "$f"; printf '%s\n' "$line"; tail -n +2 "$f"; } > "$f.spdx.tmp"
      ;;
    *)
      { printf '%s\n' "$line"; cat "$f"; } > "$f.spdx.tmp"
      ;;
  esac
  mv "$f.spdx.tmp" "$f"
  added=$((added + 1))
}

PRUNE=( -path '*/target/*' -o -path '*/node_modules/*' -o -path '*/.git/*' \
        -o -path '*/vendor/*' -o -path '*/dist/*' -o -path '*/build/*' )

# `//` comment style.
while IFS= read -r -d '' f; do process "$f" "// " ""; done < <(
  find . \( "${PRUNE[@]}" \) -prune -o \
    -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.js' -o -name '*.mjs' \) -print0
)
# `<!-- ... -->` comment style for Svelte components.
while IFS= read -r -d '' f; do process "$f" "<!-- " " -->"; done < <(
  find . \( "${PRUNE[@]}" \) -prune -o \
    -type f -name '*.svelte' -print0
)

echo "SPDX sweep: ${added} $([ "$DRY_RUN" = 1 ] && echo 'would be added' || echo 'added'), ${skipped} already tagged"

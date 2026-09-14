#!/bin/bash
# ralph-models.sh — show or set the per-host model configuration a ralph
# campaign reads (ralph/models.env), and restart its launchd job so worker,
# review and supervisor-resolution sessions all pick the change up.
#
#   scripts/ralph-models.sh                      # show the current file
#   scripts/ralph-models.sh --model M --review-model R --variant V
#   scripts/ralph-models.sh --label domains --no-restart --model M
#
# Keys left out of a set keep their current value. Loop flags still override
# the file for ad-hoc runs; the file is what a detached job reads.
set -u
WORKDIR="."
LABEL=""
RESTART=1
MODEL=""
REVIEW_MODEL=""
VARIANT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --no-restart) RESTART=0; shift ;;
    --model) MODEL="$2"; shift 2 ;;
    --review-model) REVIEW_MODEL="$2"; shift 2 ;;
    --variant) VARIANT="$2"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "ralph-models: unknown flag $1" >&2; exit 2 ;;
  esac
done
cd "$WORKDIR" || exit 2
F="ralph/models.env"
mkdir -p ralph

show() {
  printf 'models: %s\n' "$F"
  printf '  MODEL=%s\n  REVIEW_MODEL=%s\n  VARIANT=%s\n' \
    "${1:-<unset>}" "${2:-<unset>}" "${3:-<unset>}"
}

cur_model=""; cur_review=""; cur_variant=""
if [ -f "$F" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      MODEL) cur_model="$v" ;;
      REVIEW_MODEL) cur_review="$v" ;;
      VARIANT) cur_variant="$v" ;;
    esac
  done < <(grep -E '^(MODEL|REVIEW_MODEL|VARIANT)=' "$F")
fi

if [ -z "$MODEL" ] && [ -z "$REVIEW_MODEL" ] && [ -z "$VARIANT" ]; then
  show "$cur_model" "$cur_review" "$cur_variant"
  exit 0
fi

[ -n "$MODEL" ] || MODEL="$cur_model"
[ -n "$REVIEW_MODEL" ] || REVIEW_MODEL="$cur_review"
[ -n "$VARIANT" ] || VARIANT="$cur_variant"

tmp="$F.tmp.$$"
{
  echo "# ralph per-host model configuration (gitignored); written by scripts/ralph-models.sh"
  echo "MODEL=$MODEL"
  echo "REVIEW_MODEL=$REVIEW_MODEL"
  echo "VARIANT=$VARIANT"
} > "$tmp" && mv "$tmp" "$F"

if git rev-parse --git-dir >/dev/null 2>&1; then
  mkdir -p .git/info
  grep -qxF "$F" .git/info/exclude 2>/dev/null || echo "$F" >> .git/info/exclude
fi

show "$MODEL" "$REVIEW_MODEL" "$VARIANT"

if [ "$RESTART" -eq 1 ] && [ -n "$LABEL" ]; then
  l="dev.ralph.$(basename "$PWD")-$LABEL"
  if launchctl print "gui/$(id -u)/$l" >/dev/null 2>&1; then
    launchctl kickstart -k "gui/$(id -u)/$l" >/dev/null
    echo "restarted $l (any in-flight session was killed)"
  else
    echo "$l is not loaded — the change applies on the next start"
  fi
fi

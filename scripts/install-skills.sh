#!/bin/sh
# install-skills.sh — make this repo's skills available in EVERY project on
# this machine, for every harness, without copying them.
#
# The skills are authored once under <repo>/.claude/skills/ and stay git-tracked
# there. This script only symlinks them into each harness's GLOBAL skill
# location, so a skill edit lands in one file and all three harnesses see it
# immediately — no sync step, nothing to drift.
#
# Global locations, per harness (project-local discovery is unaffected):
#   ~/.claude/skills/        Claude Code
#   ~/.agents/skills/        pi (documented global) and opencode (scans .agents)
#   ~/.pi/agent/skills/      pi's own global dir, belt and braces
#
# Idempotent: re-running repoints existing links. Removes only links this
# script owns (a symlink pointing into this repo), never a real directory.
#
#   sh scripts/install-skills.sh          # install/refresh
#   sh scripts/install-skills.sh --list   # show what is linked where
#   sh scripts/install-skills.sh --remove # unlink
set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$REPO/.claude/skills"
TARGETS="$HOME/.claude/skills $HOME/.agents/skills $HOME/.pi/agent/skills"
MODE="${1:-install}"

[ -d "$SRC" ] || { echo "install-skills: no $SRC" >&2; exit 1; }

# A link is ours only if it resolves inside this repo. Anything else is the
# operator's and is left alone rather than clobbered.
owned() {
    [ -L "$1" ] || return 1
    case "$(cd "$(dirname "$1")" && readlink "$1")" in "$REPO"/*) return 0 ;; esac
    return 1
}

for dir in $TARGETS; do
    [ "$MODE" = "install" ] && mkdir -p "$dir"
    [ -d "$dir" ] || continue
    for skill in "$SRC"/*/; do
        name="$(basename "$skill")"
        link="$dir/$name"
        case "$MODE" in
        install)
            if [ -e "$link" ] && ! owned "$link"; then
                echo "  skip   $link (not ours — a real directory or foreign link)"
                continue
            fi
            ln -sfn "$SRC/$name" "$link"
            echo "  link   $link -> $SRC/$name"
            ;;
        --remove)
            owned "$link" && { rm -f "$link"; echo "  unlink $link"; }
            ;;
        --list)
            [ -L "$link" ] && echo "  $link -> $(readlink "$link")"
            ;;
        esac
    done
done

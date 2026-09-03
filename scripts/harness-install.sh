#!/bin/sh
# harness-install.sh — install this repo's agent tooling into another repo.
#
# The hooks, skills and settings here are already harness-neutral and
# repo-neutral (each hook reads a JSON envelope on stdin and fails open); the
# only asset that is actively WRONG after a plain copy is AGENTS.md, which is
# half commonwealth-ai specifics. So this script copies the neutral set
# verbatim and assembles AGENTS.md from the regions marked
# `<!-- portable:start -->` / `<!-- portable:end -->` in the source AGENTS.md.
# That marking is why there is no second copy of the compass to drift: edit
# AGENTS.md here, re-run this, and every installed repo picks the change up.
#
#   sh scripts/harness-install.sh ../my-new-project
#   sh scripts/harness-install.sh ../my-new-project --with-comaintainer
#   sh scripts/harness-install.sh ../my-new-project --dry-run
#
# Idempotent. Re-running refreshes the hooks and the AGENTS.md core block and
# leaves everything you wrote below the core marker untouched.
set -eu

SRC="$(cd "$(dirname "$0")/.." && pwd)"
DEST=""
WITH_CO=0
DRY=0

for arg in "$@"; do
    case "$arg" in
    --with-comaintainer) WITH_CO=1 ;;
    --dry-run) DRY=1 ;;
    -*) echo "harness-install: unknown flag $arg" >&2; exit 2 ;;
    *) DEST="$arg" ;;
    esac
done

[ -n "$DEST" ] || { echo "usage: harness-install.sh <target-repo> [--with-comaintainer] [--dry-run]" >&2; exit 2; }
[ -d "$DEST" ] || { echo "harness-install: no such directory: $DEST" >&2; exit 1; }
DEST="$(cd "$DEST" && pwd)"
[ "$DEST" != "$SRC" ] || { echo "harness-install: target is the source repo" >&2; exit 1; }

say() { echo "  $*"; }
run() { [ "$DRY" -eq 1 ] && { say "would: $*"; return 0; }; "$@"; }

echo "harness-install: $SRC -> $DEST"
[ "$DRY" -eq 1 ] && echo "  (dry run — nothing is written)"

# ── hooks + statusline ────────────────────────────────────────────────────
# Copied whole. Every one of these fails open (exit 0 on anything unexpected)
# and guards on file extension, so they are inert in a repo whose language
# they do not know rather than noisy or wrong.
run mkdir -p "$DEST/.claude/hooks" "$DEST/.claude/scripts"
for f in "$SRC"/.claude/hooks/*.sh "$SRC"/.claude/hooks/*.py; do
    [ -f "$f" ] || continue
    run cp "$f" "$DEST/.claude/hooks/"
    say "hook     .claude/hooks/$(basename "$f")"
done
run cp "$SRC/.claude/scripts/read-budget-statusline.py" "$DEST/.claude/scripts/"
say "status   .claude/scripts/read-budget-statusline.py"

# The hook self-tests deliberately do NOT come along: run-all.sh asserts on
# `target/debug/sovereign-cli`, so it only runs inside this source tree. A
# copied suite that cannot run is worse than no suite — it reads as coverage.
# The smoke check printed at the end is the portable substitute.

# ── settings + MCP wiring ─────────────────────────────────────────────────
# settings.json refers to hooks only through $CLAUDE_PROJECT_DIR, so it is
# portable as-is. An existing one is preserved — merging two hook graphs by
# hand is worse than telling you about it.
if [ -e "$DEST/.claude/settings.json" ]; then
    say "skip     .claude/settings.json (exists — merge by hand from $SRC/.claude/settings.json)"
else
    run cp "$SRC/.claude/settings.json" "$DEST/.claude/settings.json"
    say "wire     .claude/settings.json"
fi
[ -e "$DEST/.mcp.json" ] || { run cp "$SRC/.mcp.json" "$DEST/.mcp.json"; say "wire     .mcp.json"; }

# ── opencode ──────────────────────────────────────────────────────────────
run mkdir -p "$DEST/.opencode/plugins"
run cp "$SRC/.opencode/plugins/sovereign-hooks.ts" "$DEST/.opencode/plugins/"
say "hook     .opencode/plugins/sovereign-hooks.ts"
if [ -e "$DEST/.opencode/opencode.json" ]; then
    say "skip     .opencode/opencode.json (exists)"
else
    # Only the MCP + plugin wiring travels. The provider block in this repo's
    # config names local models and does not belong in someone else's tree.
    if [ "$DRY" -eq 0 ]; then
        cat > "$DEST/.opencode/opencode.json" <<'JSON'
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["./plugins/sovereign-hooks.ts"],
  "mcp": {
    "sovereign": {
      "type": "remote",
      "url": "http://localhost:9741/mcp"
    }
  }
}
JSON
    fi
    say "wire     .opencode/opencode.json"
fi

# ── pi ────────────────────────────────────────────────────────────────────
if [ -d "$SRC/.pi/extensions/sovereign-hooks" ]; then
    run mkdir -p "$DEST/.pi/extensions/sovereign-hooks"
    run cp "$SRC/.pi/extensions/sovereign-hooks/index.ts" "$DEST/.pi/extensions/sovereign-hooks/"
    [ -e "$DEST/.pi/settings.json" ] || {
        [ "$DRY" -eq 0 ] && printf '{\n  "skills": ["../.claude/skills"]\n}\n' > "$DEST/.pi/settings.json"
        :
    }
    say "hook     .pi/extensions/sovereign-hooks/index.ts"
fi

# ── skills ────────────────────────────────────────────────────────────────
# fieldglass and fleet-report call binary verbs and one script; comaintainer
# names 16 repo-relative scripts and is opt-in for that reason.
run mkdir -p "$DEST/.claude/skills"
for s in fieldglass fleet-report; do
    [ -d "$SRC/.claude/skills/$s" ] || continue
    run cp -R "$SRC/.claude/skills/$s" "$DEST/.claude/skills/"
    say "skill    .claude/skills/$s"
done
run mkdir -p "$DEST/scripts"
run cp "$SRC/scripts/fleet-report.py" "$DEST/scripts/" 2>/dev/null || true
run cp "$SRC/scripts/run-if-stale.sh" "$DEST/scripts/" 2>/dev/null || true

if [ "$WITH_CO" -eq 1 ]; then
    run cp -R "$SRC/.claude/skills/comaintainer" "$DEST/.claude/skills/"
    say "skill    .claude/skills/comaintainer"
    # The 14 co-* files with at most one repo reference. The five left behind
    # (co-lineage, co-backlog, co-order, co-campaign, co-closeout) are wired
    # to this repo's quality/ and .sovereign/features/ layout.
    for f in co-apply.py co-arch.py co-console.py co-drift.py co-field.py co-role.py \
             co-backlog-producer.sh co-boot-block.sh co-directive-log.sh \
             co-mesh-drill.sh co-review.sh co-sweep.sh co_liveness.py co_notes.py; do
        [ -f "$SRC/scripts/$f" ] || continue
        run cp "$SRC/scripts/$f" "$DEST/scripts/"
    done
    say "scripts  14 co-* files (5 repo-coupled ones left behind — see SKILL.md)"
fi

# ── AGENTS.md ─────────────────────────────────────────────────────────────
# Assembled from the marked regions, never copied whole.
CORE_START='<!-- svrn:core start — generated by harness-install.sh. Edit the source AGENTS.md, re-run. -->'
CORE_END='<!-- svrn:core end -->'

if [ "$DRY" -eq 0 ]; then
    python3 - "$SRC/AGENTS.md" "$DEST/AGENTS.md" "$CORE_START" "$CORE_END" <<'PY'
import re, sys, os
src, dst, start, end = sys.argv[1:5]
text = open(src).read()
blocks = re.findall(r'^<!-- portable:start[^>]*-->\n(.*?)^<!-- portable:end -->',
                    text, re.M | re.S)
if not blocks:
    sys.exit("harness-install: no portable:start markers in " + src)
core = start + "\n\n" + "\n".join(b.rstrip() + "\n" for b in blocks) + "\n" + end

if os.path.exists(dst):
    old = open(dst).read()
    if start in old and end in old:
        new = re.sub(re.escape(start) + r'.*?' + re.escape(end), lambda _: core,
                     old, flags=re.S)
    else:
        new = core + "\n\n" + old
else:
    name = os.path.basename(os.path.dirname(os.path.abspath(dst))) or "this project"
    new = (f"# Agent instructions — {name}\n\n" + core + "\n\n"
           "<!-- Everything below is yours; harness-install.sh never touches it. -->\n\n"
           "## This project\n\n"
           "_Describe the build, the test command, the layout, and anything an\n"
           "agent would otherwise guess wrong. The block above is the shared\n"
           "working standard and is regenerated on every install._\n")
open(dst, "w").write(new)
print(f"  compass  AGENTS.md ({len(blocks)} portable blocks, {len(core.splitlines())} lines)")
PY
else
    say "would: assemble AGENTS.md from portable blocks"
fi

echo
echo "Next, in $DEST:"
echo "  svrn init      # index + register with the daemon on :9741"
echo
echo "Smoke-check the hooks (should print a boot block and a notes index):"
echo "  echo '{\"session_id\":\"smoke\",\"source\":\"startup\",\"prompt\":\"hi\"}' \\"
echo "    | sh .claude/hooks/session-boot.sh"

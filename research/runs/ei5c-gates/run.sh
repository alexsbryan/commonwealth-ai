#!/usr/bin/env bash
# ei-5c-seed-race — the mechanical gates, as one unit.
#
# Why a run-request and not four Bash calls: the workspace build is the
# dependency of leg 3 (concept-gate relays through the `sovereign-cli-dev`
# sibling and reads COULD-NOT-JUDGE without a fresh one — exit 3, not a
# finding), and a cold build plus lint --full plus the sweep is well past the
# harness's 10-minute clamp. Legs are ordered so the expensive shared build is
# paid once.
#
# Every leg writes its own rc marker; the DONE marker is written on SIGTERM too,
# so a killed run is still a verdict rather than a silence.
set -u
WT=/home/alexbryan/dev/ei5c-wt
OUT="$WT/runs/ei5c-gates/out"
mkdir -p "$OUT"
rm -f "$OUT"/*.rc "$OUT"/DONE

finish() { echo "$(date -Is)" > "$OUT/DONE"; }
trap finish EXIT TERM INT

box() {
  { echo "== box $1 $(date -Is)";
    free -g | sed -n '1,2p';
    df -h /home | tail -1;
    echo "-- builds in flight (should be empty but this run's own):";
    pgrep -af 'cargo|rustc' | grep -v pgrep | head -5;
  } > "$OUT/box-$1.txt" 2>&1
}

leg() { # leg <name> <cmd...>
  local name="$1"; shift
  echo "== $name $(date -Is)" >> "$OUT/log.txt"
  ( cd "$WT" && "$@" ) > "$OUT/$name.txt" 2>&1
  local rc=$?
  echo "$rc" > "$OUT/$name.rc"
  echo "== $name rc=$rc $(date -Is)" >> "$OUT/log.txt"
  return 0    # never abort the unit; a red leg is a verdict the next leg still wants reported
}

box before

# PREFLIGHT, before the long step: the two things that make legs 3-5 meaningless
# if absent. Reported, never assumed.
{ echo "toolchain: $(cargo --version 2>&1)";
  echo "branch:    $(git -C "$WT" branch --show-current)";
  echo "tip:       $(git -C "$WT" rev-parse HEAD)";
  echo "dirty:     $(git -C "$WT" status --porcelain | wc -l) path(s)";
} > "$OUT/preflight.txt" 2>&1

# 1. the shared build. `--bins` + dev-tools is the feature contract AGENTS.md
#    names; without dev-tools the dispatcher is silently downgraded.
leg build cargo build --bins --features sovereign-cli/dev-tools,corpus-engine/treesitter

# 2. concept-gate — needs leg 1's sibling; exit 3 here means COULD-NOT-JUDGE.
leg concept-gate cargo run --manifest-path "$WT/corpus-engine/xtask/Cargo.toml" -- concept-gate

# 3. the compile gate, whole workspace.
leg lint ./scripts/sovereign-lint.sh --human --full

# 4. the test sweep, scoped to the crates this order changed plus their
#    dependents that carry its guards.
leg test ./scripts/sovereign-test.sh --human \
    --package corpus-engine --package corpus-engine-vocab \
    --package sovereign-core --package corpus-mcp --package sovereign-cli-llm

box after

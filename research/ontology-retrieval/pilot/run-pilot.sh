#!/usr/bin/env bash
# The stage-0 pilot: prove checks I1-I7 have teeth, on the installed
# chaos-secret-agent corpus, before any GPU meter starts.
#
#   research/ontology-retrieval/pilot/run-pilot.sh [--dry-run]
#
# It builds the eval binaries, runs the four arms this host can build (ONE run
# each), prints the seven-row check table clean, then prints it again with a
# fault planted for each of the seven. Exit 0 only when the clean table reads
# what the pilot declared it would and every plant was caught.
#
# NO BAR IS READ HERE. The pilot is instrument evidence: it says the checks
# fire, never that the ontology helped. That is the study's job, on the corpora
# the pre-reg names.
#
# Two checks cannot run on this host and the pilot says so rather than dropping
# them: `ablation` needs a second index built with no `[enrichment.ontology]`,
# and `oracle` needs a corpus holding only the attesting passages. Neither is
# built here, so I3 and I5 read `never-ran`. That carve-out is written out in
# EXPECT_NEVER_RAN below and applies to those two ids ONLY — any other check
# that goes absent is a failure, because a check nobody ran makes no claim and
# must not ride out on an exemption written for someone else (ARCH §5, §6).
#
# --dry-run does the preflight and prints the plan, runs no eval and no cargo.
# It is what `pod_window.sh --rehearse` needs and what proves this script
# without the ~25 minutes of model calls a real pilot costs.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
REPO="$PWD"

CORPUS="${PILOT_CORPUS:-chaos-secret-agent}"
DATA_ROOT="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}"
INDEX_DIR="${PILOT_INDEX_DIR:-$DATA_ROOT/indexes/$CORPUS}"
RECIPE="${PILOT_RECIPE:-sovereign-recipes/$CORPUS/recipe.toml}"
# A reduced bank is a REHEARSAL of this script, never the pilot: the pre-reg's
# n = 20 floor is nowhere near met and no category verdict from it means
# anything. It exists because the real bank costs ~28 s per question per arm
# (measured on this host, 2026-09-19) and the script's own gates are worth
# watching fire before a GPU meter is running.
BANK="${PILOT_BANK:-research/ontology-retrieval/pilot/bank.toml}"
RUNS="${PILOT_RUNS:-research/ontology-retrieval/pilot/runs}"
RUN_ARM="research/ontology-retrieval/harness/run_arm.py"
CHECKS="research/ontology-retrieval/harness/checks.py"
DAEMON="${SOVEREIGN_DAEMON_URL:-${SVRNMESH_DAEMON_URL:-http://localhost:9741}}"

# The arms this host can build, in the pre-reg's order. `deep` carries the pool
# multiplier the pre-reg leaves to the run; run_arm.py REFUSES it unbound.
ARMS=(closed-book bare deep full)
POOL_SCALE="${PILOT_POOL_SCALE:-2}"
# I3 needs an ablation index, I5 an oracle corpus. Neither is built here.
EXPECT_NEVER_RAN="I3 I5"

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
die()  { printf 'run-pilot: %s\n' "$*" >&2; exit 2; }

# ── preflight: everything that can refuse, refuses before anything runs ──────

step "preflight"
for f in "$BANK" "$RECIPE" "$RUN_ARM" "$CHECKS"; do
    [ -f "$f" ] || die "missing $f"
done
[ -d "$INDEX_DIR" ] || die "no index dir at $INDEX_DIR — is the \`$CORPUS\` corpus installed? (PILOT_INDEX_DIR overrides)"
[ -e "$INDEX_DIR/atlas/ontology.json" ] || die "no $INDEX_DIR/atlas/ontology.json — this build carries no ontology, so I6 has nothing to census"

# The daemon must be UP and serving a model. The pilot never starts, stops or
# restarts it: on this host it is the deployed daemon and other sessions share
# it. `/healthz` is a 404 here; `/v1/models` is the liveness surface.
models_json="$(curl -s -m 5 "$DAEMON/v1/models" 2>/dev/null)"
case "$models_json" in
    *'"id"'*) say "daemon $DAEMON serving $(printf '%s' "$models_json" | grep -o '"id":"[^"]*"' | head -1)" ;;
    *) die "daemon at $DAEMON lists no model — start it before the pilot (the pilot never starts it)" ;;
esac
say "corpus   $CORPUS"
say "index    $INDEX_DIR"
say "recipe   $RECIPE"
say "bank     $BANK ($(grep -c '^\[\[questions\]\]' "$BANK") questions)"
say "runs     $RUNS"
say "arms     ${ARMS[*]} (pool scale $POOL_SCALE on \`deep\`), one run each"
say "never-ran by design: $EXPECT_NEVER_RAN"

if [ "$DRY_RUN" = 1 ]; then
    step "dry run — the resolved argv per arm"
    for arm in "${ARMS[@]}"; do
        python3 "$RUN_ARM" --arm "$arm" --bank "$BANK" --corpus "$CORPUS" \
            --index-dir "$INDEX_DIR" --recipe "$RECIPE" --out "$RUNS" \
            --run 1 --pool-scale "$POOL_SCALE" --dry-run \
            || die "arm $arm refused its own premises"
    done
    step "dry run complete — nothing was built and no question was asked"
    exit 0
fi

# ── build ────────────────────────────────────────────────────────────────────

step "build"
# `eval` dispatches out of sovereign-cli into the sovereign-cli-llm sibling, so
# both are built or the dispatcher execs a stale binary. `dev-tools` is not
# optional: without it the build silently replaces the installed dispatcher
# with an end-user one. `corpus-engine/treesitter` is the repo's real feature
# contract — a narrow build without it rebuilds corpus-engine and its
# dependents twice (AGENTS.md, "Feature-unification hygiene").
./scripts/with-cargo-lock.sh cargo build -p sovereign-cli -p sovereign-cli-llm \
    --features sovereign-cli/dev-tools,corpus-engine/treesitter \
    || die "build failed — the arms would have run a stale binary"

# ── the arms ─────────────────────────────────────────────────────────────────

rm -rf "$RUNS"
for arm in "${ARMS[@]}"; do
    step "arm $arm"
    python3 "$RUN_ARM" --arm "$arm" --bank "$BANK" --corpus "$CORPUS" \
        --index-dir "$INDEX_DIR" --recipe "$RECIPE" --out "$RUNS" \
        --run 1 --pool-scale "$POOL_SCALE" \
        || die "arm $arm refused its own premises"
done

# run_arm.py exits 0 for a run it recorded as never-ran — the admissibility is
# in the manifest, not the exit code. A pilot whose arms did not run cannot
# claim seven verdicts, so that is read here and refused.
step "arm admissibility"
python3 - "$RUNS" <<'PY' || exit 3
import json, sys
from pathlib import Path
bad = []
for mf in sorted(Path(sys.argv[1]).glob("*/run-*/manifest.json")):
    doc = json.loads(mf.read_text())
    label = f"{mf.parent.parent.name}/{mf.parent.name}"
    if doc.get("verdict") == "never-ran":
        bad.append(f"{label}: {'; '.join(doc.get('never_ran_reasons') or ['no reason recorded'])}")
    else:
        print(f"  {label}: recorded")
if bad:
    print("run-pilot: arm(s) recorded never-ran — the checks would judge nothing:",
          file=sys.stderr)
    for b in bad:
        print(f"  {b}", file=sys.stderr)
    raise SystemExit(1)
PY

# ── the seven checks, clean ──────────────────────────────────────────────────

step "checks — clean"
python3 "$CHECKS" --runs "$RUNS" --out "$RUNS"
clean_exit=$?
[ "$clean_exit" = 2 ] && die "checks refused the runs dir"

python3 - "$RUNS/checks.jsonl" $EXPECT_NEVER_RAN <<'PY' || exit 4
import json, sys
from pathlib import Path
path, exempt = Path(sys.argv[1]), set(sys.argv[2:])
rows = [json.loads(x) for x in path.read_text().splitlines()]
want = {r["check"]: ("never-ran" if r["check"] in exempt else "passed") for r in rows}
bad = [r for r in rows if r["verdict"] != want[r["check"]]]
if len(rows) != 7:
    print(f"run-pilot: {len(rows)} check row(s), expected 7", file=sys.stderr)
    raise SystemExit(1)
for r in rows:
    if r["check"] in exempt:
        print(f"  {r['check']}: never-ran by design — {r['reason']}")
if bad:
    print("run-pilot: the clean table is not what the pilot declared:", file=sys.stderr)
    for r in bad:
        print(f"  {r['check']}: {r['verdict']}, expected {want[r['check']]} — "
              f"{r['reason']}", file=sys.stderr)
    raise SystemExit(1)
print(f"  clean: {7 - len(exempt)} passed, {len(exempt)} never-ran by design")
PY

# ── the seven checks, each with its own fault planted ────────────────────────

step "checks — planted"
# Every plant works on a COPY; nothing under $RUNS is written. A plant that
# leaves its check green is the finding, not a flake: the enforcement does not
# enforce, and the pilot must not exit 0 through it.
python3 "$CHECKS" --runs "$RUNS" --out "$RUNS" --plant all || exit 5

step "verdict"
say "pilot: 7 checks issued clean ($EXPECT_NEVER_RAN never-ran by design), 7 faults planted and caught"
exit 0

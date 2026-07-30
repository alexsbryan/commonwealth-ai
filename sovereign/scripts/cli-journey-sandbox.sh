#!/usr/bin/env bash
# cli-journey-sandbox.sh — boot a throwaway daemon and run the MUTATING half
# of the journey manifest against it.
#
# WHY THIS EXISTS. `cli-journey-verify.sh` in its default read-only mode can
# only ever verify a journey's read-only PREFIX. On the first live run
# (2026-07-28) that meant 13 of 15 runnable journeys reported `partial`: the
# steps that actually carry the meaning — `corpus install`, `corpus remove`
# and the "prove it is gone" assertion after it, `notes add` then read it
# back, `claim` then `claim release` — were all skipped, because running them
# against the operator's real ~/.sovereign is destructive. So the assertions
# the journey layer was BUILT for were the exact ones never executed.
#
# This script supplies the sandbox that the runner deliberately refuses to
# invent for itself, and then hands off. It owns the boot; the runner owns
# the journeys. That split is why the runner stays safe to point at any
# daemon, including a production one.
#
# The isolation is lifted wholesale from scripts/daemon-soak.sh, which has
# been running this exact pattern since 2026-07-18 — private netns, throwaway
# HOME, mDNS off, iroh kill-switched, non-default port, every spawned pid
# tracked and killed by pid (never pkill-by-name: the operator's live daemon
# runs the same binary path).
#
# ── usage ────────────────────────────────────────────────────────────────
#   sovereign/scripts/cli-journey-sandbox.sh                # all tiers
#   sovereign/scripts/cli-journey-sandbox.sh --tier 1
#   sovereign/scripts/cli-journey-sandbox.sh --journey corpus-lifecycle
#   JOURNEY_CORPUS=sep sovereign/scripts/cli-journey-sandbox.sh
#
# Env: JOURNEY_PORT (default 19741), JOURNEY_CORPUS (see the note below),
#      PRIMARY_GGUF / EMBED_GGUF (default to the small soak models),
#      READY_BUDGET_SECS (default 180), KEEP_HOME=1 to keep the sandbox.
#
# ── what the summary counts ──────────────────────────────────────────────
# Six buckets, because a journey count is not coverage and the runner's exit
# code cannot tell these apart on its own:
#   proved         entered, every declared step ran, nothing failed, and at
#                  least one step asserted something
#   partial        entered and ran, but a precondition was skipped
#   not attempted  never entered — `skip_live` (the author's scope) or a
#                  declared `needs` this lane `--lacks` (this lane's gap, and
#                  what cli-journey-nightly.sh runs read-only instead)
#   unproven       entered, ran, and not one step asserted anything: the
#                  commands were invoked and nobody looked at the output
#   vacuous        entered and executed ZERO steps: needs a fixture
#   failed         a step asserted something untrue
#
# Until 2026-07-29 the first bucket absorbed the middle two, so this lane
# reported `30 ok` for a run where 19 of 32 journeys had executed nothing.
#
# ── exit codes ───────────────────────────────────────────────────────────
#   0  every journey proved something and none failed
#   1  a journey failed, or the daemon could not be kept alive
#   2  misuse (unbuilt binaries, missing models, port in use, bad --journey)
#   4  at least one journey proved nothing — it executed ZERO steps, or it ran
#      and asserted nothing. Nothing is broken; nothing was tested either.
#      See the coverage line for what is missing.
#
# ── on the corpus fixture ────────────────────────────────────────────────
# `{corpus}` defaults to `journey-fixture`: three small markdown documents in
# sovereign/tests/fixtures/journey-corpus, installed through a REAL recipe
# (`acquire.type = "local_file"`) that this script publishes into the sandbox
# HOME's registry. No network, no download, seconds to index.
#
# It used to be UNSET, on the reasoning that the installable catalog's
# smallest member is a ~0.5 GB download and a routine local guard should not
# fetch that behind your back. That reasoning was right about the catalog and
# wrong about the conclusion: `{corpus}` is the most demanded token in the
# manifest (25 occurrences), so leaving it unset left four journeys executing
# nothing at all and the manifest's behavioural coverage at 23%. The fix is a
# fixture that costs nothing, not an absent one.
#
# `corpus install` still runs its genuine recipe-driven path — only the bytes
# are local. Point JOURNEY_CORPUS at a catalog id (`sep`) to prove the
# download path too; that skips the fixture seeding entirely.
set -uo pipefail

# ── private netns ────────────────────────────────────────────────────────
# Re-exec into a network namespace with no route out, so a sandboxed daemon
# cannot find (or be found by) the operator's real mesh. `mesh join` hardcodes
# :9741 and the daemon has no mDNS-disable knob beyond config, so the netns is
# the load-bearing guarantee, not the config line.
if [ "${JOURNEY_SANDBOX_NS:-0}" != "1" ] && command -v unshare >/dev/null 2>&1; then
  exec unshare -r -n env JOURNEY_SANDBOX_NS=1 "$0" "$@"
fi

# `--journey` is consumed HERE, not forwarded: this script drives one journey
# per runner invocation, so passing the user's `--journey` through as well
# would put two of them on the runner's command line and the last one would
# win for every iteration.
ONLY_JOURNEY=""
declare -a PASSTHRU=()
while [ $# -gt 0 ]; do
  case "$1" in
    --journey) ONLY_JOURNEY="$2"; shift 2 ;;
    *) PASSTHRU+=("$1"); shift ;;
  esac
done
# NOT `set -- "${PASSTHRU[@]:-}"`: under `set -u` that expands an EMPTY array
# to one empty-string argument, which the runner then rejects as `unknown
# flag: `. Guard on length instead.
if [ ${#PASSTHRU[@]} -gt 0 ]; then set -- "${PASSTHRU[@]}"; else set --; fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
RUNNER="$HERE/cli-journey-verify.sh"
[ -x "$RUNNER" ] || { echo "sandbox: $RUNNER not executable" >&2; exit 2; }

# Loopback is down inside a fresh netns; every probe below targets 127.0.0.1.
ip link set lo up 2>/dev/null || true

PORT="${JOURNEY_PORT:-19741}"
INTERNAL_PORT=$((PORT + 1))
READY_BUDGET_SECS="${READY_BUDGET_SECS:-180}"
DAEMON_BIN="${DAEMON_BIN:-$REPO_ROOT/target/debug/sovereign-cli-daemon}"
CLI_BIN="${SOVEREIGN_BIN:-$REPO_ROOT/target/debug/sovereign-cli}"
for f in "$DAEMON_BIN" "$CLI_BIN"; do
  [ -x "$f" ] || { echo "sandbox: missing $f — cargo build --bins --features sovereign-cli/dev-tools" >&2; exit 2; }
done

# ── model resolution ─────────────────────────────────────────────────────
# A single hardcoded path per model is a single point of failure for an
# UNATTENDED lane: the nightly timer's whole job is to run when nobody is
# watching, and "exit 2, missing model" is a night that tested nothing. The
# repo also has two model directories — <root>/models (nested, one .gguf per
# subdir) and <root>/sovereign/models (flat) — so which one holds a usable
# model is a per-host fact, not a constant.
#
# So: an ordered candidate list, the historical defaults FIRST so this is a
# strict superset of the previous behaviour, and the resolved choice PRINTED.
# A sandbox that silently picked a different model than you assumed is a
# debugging trap; naming it costs one line.
#
# The journeys mostly exercise the CLI SURFACE rather than inference quality,
# but "mostly" is not "never": `chat inspect` asserts stdout_non_empty, so the
# primary slot has to be a model that can actually GENERATE, not merely load.
#
# That is why Bonsai-8B-Q1_0 is no longer first. It is a 1-bit quant kept for
# daemon-soak, where only boot mattered; asked a real question it emits EOS
# immediately and returns an empty completion, which reads as a broken journey
# rather than an unusable fixture model. Prefer a small well-quantized model
# and keep the Q1 as a last resort so a host that has only it still boots.
pick_model() { # var-name description candidate...
  local var="$1" desc="$2"; shift 2
  local cur="${!var:-}" c
  if [ -n "$cur" ]; then
    [ -f "$cur" ] || { echo "sandbox: $var=$cur does not exist" >&2; return 1; }
    echo "sandbox: $desc = $(basename "$cur") (from \$$var)"
    return 0
  fi
  for c in "$@"; do
    if [ -f "$c" ]; then
      printf -v "$var" '%s' "$c"
      echo "sandbox: $desc = $(basename "$c")"
      return 0
    fi
  done
  echo "sandbox: no $desc found. Tried:" >&2
  printf '           %s\n' "$@" >&2
  echo "         Set $var to a .gguf on this host." >&2
  return 1
}

M="$REPO_ROOT/models"          # nested layout, holds the historical defaults
S="$REPO_ROOT/sovereign/models" # flat layout, the working model collection
pick_model PRIMARY_GGUF "primary model" \
  "$S/Qwen3.5-2B.Q6_K.gguf" \
  "$S/Qwen3.5-0.8B-UD-Q6_K_XL.gguf" \
  "$S/Qwen3.5-4B.Q6_K.gguf" \
  "$M/bonsai-8b.gguf/Bonsai-8B-Q1_0.gguf" || exit 2
pick_model EMBED_GGUF "embed model" \
  "$M/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf" \
  "$S/Qwen3-Embedding-0.6B-Q8_0.gguf" \
  "$S/qwen-embedding-0.6b.gguf" || exit 2

SANDBOX_HOME="$(mktemp -d /tmp/cli-journey.XXXXXX)"
LOG_DIR="$SANDBOX_HOME/.sovereign/logs"
mkdir -p "$SANDBOX_HOME/.sovereign" "$LOG_DIR"

cat > "$SANDBOX_HOME/.sovereign/config.toml" <<EOF
[models]
primary = "$PRIMARY_GGUF"
embed = "$EMBED_GGUF"
context_size = 4096

[daemon]
client_port = $PORT
internal_port = $INTERNAL_PORT

[data]
dir = "$SANDBOX_HOME/.sovereign"

[discovery]
mdns = false
EOF

DAEMON_PID=""
MCP_PID=""
RC=1
JSONL="$SANDBOX_HOME/journeys.jsonl"
cleanup() {
  # Kill by tracked pid only. `pkill sovereign-cli-daemon` here would take
  # down the operator's own daemon, which runs the same binary path.
  if [ -n "$DAEMON_PID" ]; then
    kill -9 "$DAEMON_PID" 2>/dev/null
    wait "$DAEMON_PID" 2>/dev/null
  fi
  if [ -n "$MCP_PID" ]; then
    kill -9 "$MCP_PID" 2>/dev/null
    wait "$MCP_PID" 2>/dev/null
  fi
  if [ "$RC" = "0" ] && [ "${KEEP_HOME:-0}" != "1" ]; then
    rm -rf "$SANDBOX_HOME"
  else
    echo
    echo "sandbox: kept $SANDBOX_HOME for triage"
    echo "         daemon stderr: $LOG_DIR/journey-daemon.err"
  fi
}
trap cleanup EXIT

echo "cli-journey-sandbox: HOME=$SANDBOX_HOME port=$PORT"
echo "                     primary=$(basename "$PRIMARY_GGUF")"
echo

probe() { curl -sf -m 2 "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1; }

if probe; then
  echo "sandbox: something already answers on :$PORT — pick another JOURNEY_PORT" >&2
  exit 2
fi

# Bring the daemon up if it is not already serving. Idempotent, because
# journeys legitimately STOP the daemon: `first-run` ends in `daemon stop`
# and `daemon-triage` restarts it — those are real user flows, and in the
# read-only lane they were simply skipped. Once the mutating lane runs them
# for real, every journey ordered after `first-run` was talking to a daemon
# that a previous journey had correctly shut down, and failed for a reason
# that had nothing to do with what it was testing.
ensure_daemon() {
  probe && return 0
  # Direct `env … cmd &` so $! IS the daemon (env execs in place). Backgrounding
  # a shell FUNCTION makes $! the subshell, and the kill then orphans a daemon
  # that keeps serving — daemon-soak.sh hit exactly that on its first run.
  env HOME="$SANDBOX_HOME" SOVEREIGN_IROH=off RUST_BACKTRACE=1 \
    "$DAEMON_BIN" daemon run \
    >>"$LOG_DIR/journey-daemon.out" 2>>"$LOG_DIR/journey-daemon.err" &
  DAEMON_PID=$!
  local start; start=$(date +%s)
  while :; do
    probe && { echo "sandbox: daemon up in $(( $(date +%s) - start ))s (pid $DAEMON_PID)"; return 0; }
    # A dead daemon will never answer — fail now rather than burn the budget.
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "sandbox: daemon exited during boot" >&2
      tail -20 "$LOG_DIR/journey-daemon.err" >&2
      return 1
    fi
    if (( $(date +%s) - start >= READY_BUDGET_SECS )); then
      echo "sandbox: daemon not ready within ${READY_BUDGET_SECS}s" >&2
      tail -20 "$LOG_DIR/journey-daemon.err" >&2
      return 1
    fi
    sleep 1
  done
}

ensure_daemon || exit 1
echo

# ── seed the fixture corpus recipe ───────────────────────────────────────
# Publishes journey-corpus.recipe.toml into the SANDBOX HOME's user registry
# with the fixture directory's absolute path substituted. The committed recipe
# keeps a @FIXTURE_DIR@ placeholder so it never carries one machine's paths.
#
# Only the recipe is registered here — `corpus install` is left to the journey
# that asserts it, because pre-installing the corpus would make
# `corpus-lifecycle` step 1 a no-op that passes without proving anything.
FIXTURE_DIR="$REPO_ROOT/sovereign/tests/fixtures/journey-corpus"
FIXTURE_RECIPE="$REPO_ROOT/sovereign/tests/fixtures/journey-corpus.recipe.toml"
JOURNEY_CORPUS="${JOURNEY_CORPUS:-journey-fixture}"

if [ "$JOURNEY_CORPUS" = "journey-fixture" ]; then
  if [ ! -d "$FIXTURE_DIR" ] || [ ! -f "$FIXTURE_RECIPE" ]; then
    echo "sandbox: fixture corpus missing ($FIXTURE_DIR)" >&2
    echo "         set JOURNEY_CORPUS to a catalog id, or restore the fixture." >&2
    exit 2
  fi
  RESOLVED="$SANDBOX_HOME/journey-fixture.recipe.toml"
  sed "s#@FIXTURE_DIR@#$FIXTURE_DIR#" "$FIXTURE_RECIPE" > "$RESOLVED"
  if env HOME="$SANDBOX_HOME" SOVEREIGN_NO_STALE_WARN=1 \
       "$CLI_BIN" recipe publish "$RESOLVED" >"$LOG_DIR/recipe-publish.log" 2>&1; then
    echo "sandbox: fixture corpus recipe published (journey-fixture → $(basename "$FIXTURE_DIR")/)"
  else
    # Do NOT fall through silently: the corpus steps would all report
    # `skip … no fixture` and the lane would look merely under-covered rather
    # than broken, which is the failure mode this whole layer exists to catch.
    echo "sandbox: FAILED to publish the fixture recipe — corpus journeys cannot run" >&2
    tail -20 "$LOG_DIR/recipe-publish.log" >&2
    exit 2
  fi
  echo
fi

# ── an MCP server for the mcp-interop journey ────────────────────────────
# `mcp-interop` wires an external MCP server in and confirms its tools are
# reachable. With no server to point at, all four steps skipped and the
# journey executed NOTHING.
#
# The fixture is the product's own `svrn mcp demo-server` — a real reference
# MCP server, already shipped, already covered by the `mcp` verb. That beats a
# hand-rolled stub twice over: no second implementation of the protocol to
# drift, and the journey exercises a server this repo actually supports.
MCP_PORT=$((PORT + 14))
MCP_NAME="${SOVEREIGN_JOURNEY_MCP_NAME:-demo}"
MCP_URL="${SOVEREIGN_JOURNEY_MCP_URL:-http://127.0.0.1:$MCP_PORT/mcp}"
if [ -z "${SOVEREIGN_JOURNEY_MCP_URL:-}" ]; then
  env HOME="$SANDBOX_HOME" SOVEREIGN_NO_STALE_WARN=1 \
    "$CLI_BIN" mcp demo-server --port "$MCP_PORT" \
    >"$LOG_DIR/mcp-demo.log" 2>&1 &
  MCP_PID=$!
  for _ in $(seq 1 30); do
    curl -sf -m 2 -X POST "$MCP_URL" -H 'content-type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' >/dev/null 2>&1 && break
    sleep 1
  done
  if curl -sf -m 2 -X POST "$MCP_URL" -H 'content-type: application/json' \
       -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' >/dev/null 2>&1; then
    echo "sandbox: reference MCP server up on :$MCP_PORT (as '$MCP_NAME')"
  else
    # Non-fatal: mcp-interop reverts to reporting ∅, which is the honest
    # outcome. Everything else in the lane is unaffected, so aborting the
    # whole run over one fixture would cost more coverage than it protects.
    echo "sandbox: reference MCP server did NOT come up — mcp-interop will report ∅" >&2
    tail -5 "$LOG_DIR/mcp-demo.log" >&2
    kill -9 "$MCP_PID" 2>/dev/null; MCP_PID=""; MCP_URL=""
  fi
  echo
fi

# ── a throwaway repo for the watcher-repair journey ──────────────────────
# `watcher-repair` registers an orphaned repo with the daemon's freshness
# pipeline and proves it shows up in `project list`. It used to reuse
# `{corpus}` — the KNOWLEDGE-corpus token — as a project id. Two distinct
# namespaces sharing one token, and the step silently registered the derived
# id instead, so step [3] asserted an id nothing had created.
#
# A distinct id alone does NOT fix it. `ProjectRegistry::nested_conflict`
# (sovereign-mesh/src/projects.rs:386) treats an identical root under a
# different name as a collision, so registering a second id against THIS repo
# would be refused as soon as any earlier journey — `code-intel-lifecycle`
# step [0] — had registered it. The journey therefore needs its own ROOT, not
# just its own name, which also makes it independent of manifest order.
#
# A git repo, because the registry's freshness pipeline polls git HEAD; a
# rootless directory would register but never be a realistic orphan.
PROJECT_ID="${SOVEREIGN_JOURNEY_PROJECT:-journey-project}"
PROJECT_ROOT="${SOVEREIGN_JOURNEY_PROJECT_ROOT:-$SANDBOX_HOME/journey-project}"
if [ -z "${SOVEREIGN_JOURNEY_PROJECT_ROOT:-}" ]; then
  mkdir -p "$PROJECT_ROOT/src"
  cat >"$PROJECT_ROOT/src/lib.rs" <<'RS'
// Minimal source so the project has something a code index could walk.
pub fn journey_fixture_marker() -> &'static str {
    "journey-project"
}
RS
  if git -C "$PROJECT_ROOT" init -q 2>/dev/null \
     && git -C "$PROJECT_ROOT" add -A 2>/dev/null \
     && git -C "$PROJECT_ROOT" -c user.email=journey@localhost \
            -c user.name=journey commit -qm "journey fixture" 2>/dev/null; then
    echo "sandbox: throwaway project repo seeded ($PROJECT_ID → $PROJECT_ROOT)"
  else
    # Non-fatal, same posture as the MCP fixture: watcher-repair reverts to
    # reporting a skip, which is the honest outcome, and the rest of the lane
    # is unaffected.
    echo "sandbox: could NOT seed the project repo — watcher-repair will skip" >&2
    PROJECT_ROOT=""
  fi
  echo
fi

# A tiny spec whose claims describe the seeded project repo, so spec-check can
# drive facts → spec-intel → check-spec end to end. {claims} is where
# `enrich spec-intel` writes: <data.dir>/specs/<corpus>/<spec-stem>/claims.json
# (observed 2026-07-30 — spec-intel's own help omits the <corpus> segment), and
# data.dir in a fresh sandbox is $HOME/.sovereign because that is what
# `setup --yes` writes into config.toml. The stem in CLAIMS_FILE must match
# SPEC_FILE's basename. Exported only when the project repo seeded
# (spec-check's first step needs {project_root}).
SPEC_FILE="${SOVEREIGN_JOURNEY_SPEC:-$SANDBOX_HOME/journey-spec.md}"
CLAIMS_FILE="${SOVEREIGN_JOURNEY_CLAIMS:-$SANDBOX_HOME/.sovereign/specs/$JOURNEY_CORPUS/journey-spec/claims.json}"
if [ -z "${SOVEREIGN_JOURNEY_SPEC:-}" ] && [ -n "$PROJECT_ROOT" ]; then
  cat >"$SPEC_FILE" <<'MD'
# journey-project spec

## Fixture marker

The `journey-project` library exposes one public function,
`journey_fixture_marker`, which takes no arguments and returns the static
string "journey-project". The marker exists so a code index built over this
repository has at least one function definition and one string literal to
report, and callers rely on the returned string equalling the project id
exactly — renaming the project without updating the marker is a drift bug.
MD
  echo "sandbox: spec fixture seeded ($SPEC_FILE)"
  echo
fi

# Hand off. SOVEREIGN_JOURNEY_ISOLATED=1 is this script asserting what it has
# actually provided — a throwaway HOME on a non-default port in a private
# netns. It is the runner's safety interlock, and the only place in the repo
# entitled to set it.
# What THIS LANE cannot supply. Two facts about a throwaway sandbox, stated
# once:
#
#   operator-home  the operator's real HOME — Claude Code transcripts under
#                  ~/.claude/projects, an accumulated notes db, a drift report
#                  on disk. A fresh mktemp HOME has none by construction, so a
#                  sandbox run of those journeys can only report a FALSE
#                  failure.
#   indexed-repo   a live code index + SCIP graph. Building one needs
#                  rust-analyzer and minutes, and a failed SCIP export wipes
#                  the graph it was replacing — not something to do per run.
#
# This used to be `--exclude session-continuity --exclude context-spend-audit`:
# two journey ids and one shared prose reason, hardcoded here, invisible from
# the manifest, and needing a hand-edit for every future journey with the same
# requirement. The journeys now DECLARE `needs` and the runner drops them with
# the manifest's own reason — so this lane and the read-only operator lane
# partition the manifest from one source of truth. What the sandbox lacks is
# exactly what cli-journey-nightly.sh then runs read-only against the real
# daemon; nothing is dropped by both, which is the property that matters.
SANDBOX_LACKS=(--lacks operator-home --lacks indexed-repo)

# One journey per runner invocation, with a daemon liveness check between
# them. A journey is supposed to be an INDEPENDENT claim about a use case;
# running the whole manifest in one pass made every journey ordered after
# `first-run` depend on `first-run` having left the daemon running, which it
# deliberately does not. Per-journey invocation costs a few seconds of
# process startup and buys back that independence.
#
# HOME state (installed corpora, registered projects, notes) still carries
# across journeys on purpose — that is one operator's machine over a
# session, which is the thing being modelled.
run_one() { # $1 = journey id; remaining = passthrough flags
  local jid="$1"; shift
  env HOME="$SANDBOX_HOME" \
      SOVEREIGN_IROH=off \
      SOVEREIGN_LIVE_JOURNEYS=1 \
      SOVEREIGN_LIVE_STRICT=1 \
      SOVEREIGN_JOURNEY_ISOLATED=1 \
      SOVEREIGN_BIN="$CLI_BIN" \
      SOVEREIGN_DAEMON_URL="http://127.0.0.1:$PORT" \
      SOVEREIGN_JOURNEY_OUT="$SANDBOX_HOME/j-$jid.jsonl" \
      ${JOURNEY_CORPUS:+SOVEREIGN_JOURNEY_CORPUS="$JOURNEY_CORPUS"} \
      ${MCP_URL:+SOVEREIGN_JOURNEY_MCP_NAME="$MCP_NAME"} \
      ${MCP_URL:+SOVEREIGN_JOURNEY_MCP_URL="$MCP_URL"} \
      ${PROJECT_ROOT:+SOVEREIGN_JOURNEY_PROJECT="$PROJECT_ID"} \
      ${PROJECT_ROOT:+SOVEREIGN_JOURNEY_PROJECT_ROOT="$PROJECT_ROOT"} \
      ${PROJECT_ROOT:+SOVEREIGN_JOURNEY_SPEC="$SPEC_FILE"} \
      ${PROJECT_ROOT:+SOVEREIGN_JOURNEY_CLAIMS="$CLAIMS_FILE"} \
    "$RUNNER" --mutating "${SANDBOX_LACKS[@]}" --journey "$jid" "$@" \
    2>&1 | grep -vE '^cli-journey: |^ +steps +[0-9]|^ +coverage |^ +manifest |^$'
  return "${PIPESTATUS[0]}"
}

# Capture the plan ONCE: the journey ids to drive, and the manifest-wide step
# total used as the honest denominator in the summary. This lane drives one
# journey per runner invocation, so no single runner run can see the whole
# manifest — that number has to be computed here or not at all.
PLAN="$SANDBOX_HOME/journey-plan.tsv"
env HOME="$SANDBOX_HOME" SOVEREIGN_NO_STALE_WARN=1 "$CLI_BIN" __journey-plan 2>/dev/null > "$PLAN"
mapfile -t JOURNEY_IDS < <(awk -F'\t' '$1=="J"{print $2}' "$PLAN")
MANIFEST_STEPS="$(awk -F'\t' '$1=="S"{n++} END{print n+0}' "$PLAN")"
if [ "${#JOURNEY_IDS[@]}" = "0" ]; then
  echo "sandbox: __journey-plan emitted no journeys (is sovereign-cli built with --features dev-tools?)" >&2
  exit 2
fi
if [ -n "$ONLY_JOURNEY" ]; then
  # Fail loudly on a typo rather than reporting a vacuous "0 ok, 0 failed".
  case " ${JOURNEY_IDS[*]} " in
    *" $ONLY_JOURNEY "*) JOURNEY_IDS=("$ONLY_JOURNEY") ;;
    *) echo "sandbox: no journey named '$ONLY_JOURNEY' in the manifest" >&2; exit 2 ;;
  esac
fi

# The verdict the RUNNER recorded for a journey, or `not-attempted` when it
# never entered it at all.
#
# WHY NOT THE EXIT CODE ALONE. The runner exits 0 both for "this journey passed"
# and for "I dropped this journey whole" (declared `skip_live`, or a `needs` this
# lane `--lacks`) — and also for `partial`. So counting exit 0 as ok reported
# `30 ok, 1 vacuous, 1 failed` for a lane where NINETEEN of the 32 journeys ran
# nothing: 14 skip_live plus 5 this lane cannot supply. The step-coverage line
# below was honest the whole time (45/133), but "30 ok" is the number a human
# reads first, and it was the same vacuous-green shape this harness exists to
# kill, sitting in its own headline.
#
# The runner already writes exactly one `kind=journey` row per journey it
# entered, carrying the verdict it decided. Read that, rather than re-deriving
# a verdict here from an exit code that cannot express the difference.
verdict_of() { # jsonl-file
  local f="$1" v
  [ -s "$f" ] || { echo "not-attempted"; return 0; }
  v="$(sed -n 's/.*"kind":"journey".*"verdict":"\([a-z]*\)".*/\1/p' "$f" | tail -1)"
  echo "${v:-not-attempted}"
}

# Why a journey was not attempted, so the summary can separate the AUTHOR's
# stated scope (`skip_live` — "needs a second machine") from this LANE's gap
# (a declared `needs` a throwaway HOME cannot supply). The second set is what
# cli-journey-nightly.sh then runs read-only against the operator's daemon, so
# conflating them would hide the half somebody still owes evidence for.
#
# Asked per journey as it is decided, not counted up-front over the whole plan:
# with `--journey <id>` the plan-wide totals would describe journeys this run
# never looked at.
why_unattempted() { # jid
  case "$(awk -F'\t' -v id="$1" '$1=="J" && $2==id {print $6; exit}' "$PLAN")" in
    skip:*) echo "skip_live" ;;
    *)      echo "lacks" ;;
  esac
}
UNATT_SKIPLIVE=0; UNATT_LACKS=0

PASSED=0; FAILED=0; VACUOUS=0; PARTIAL=0; UNATTEMPTED=0; UNPROVEN=0
declare -a FAILED_IDS=() VACUOUS_IDS=() PARTIAL_IDS=() UNPROVEN_IDS=()
for jid in "${JOURNEY_IDS[@]}"; do
  # Revive first: the PREVIOUS journey may have legitimately stopped it.
  if ! ensure_daemon; then
    echo "sandbox: daemon could not be revived before $jid — aborting" >&2
    FAILED=$((FAILED + 1)); FAILED_IDS+=("$jid (daemon unrecoverable)")
    break
  fi
  run_one "$jid" "$@"; jrc=$?
  jv="$(verdict_of "$SANDBOX_HOME/j-$jid.jsonl")"
  case "$jrc" in
    # 4 is the runner's NOTHING-WAS-PROVEN exit, and it covers two distinct
    # shapes: ∅ vacuous (no step ran at all — a missing fixture) and ⊘ unproven
    # (steps ran and not one of them asserted anything — a hole in the manifest).
    # Different owners, different repairs, so read the verdict row rather than
    # collapsing them. Folding either into `ok` is the lie this harness exists to
    # kill; folding them into `failed` conflates "this is broken" with "this was
    # never tested".
    4) case "$jv" in
         unproven) UNPROVEN=$((UNPROVEN + 1)); UNPROVEN_IDS+=("$jid") ;;
         *)        VACUOUS=$((VACUOUS + 1)); VACUOUS_IDS+=("$jid") ;;
       esac ;;
    0) case "$jv" in
         pass)          PASSED=$((PASSED + 1)) ;;
         partial)       PARTIAL=$((PARTIAL + 1)); PARTIAL_IDS+=("$jid") ;;
         unproven)      UNPROVEN=$((UNPROVEN + 1)); UNPROVEN_IDS+=("$jid") ;;
         not-attempted)
           UNATTEMPTED=$((UNATTEMPTED + 1))
           if [ "$(why_unattempted "$jid")" = "skip_live" ]; then
             UNATT_SKIPLIVE=$((UNATT_SKIPLIVE + 1))
           else
             UNATT_LACKS=$((UNATT_LACKS + 1))
           fi ;;
         # vacuous already exits 4 under --mutating; anything else is new and
         # should be visible rather than folded into a pass.
         *)             PARTIAL=$((PARTIAL + 1)); PARTIAL_IDS+=("$jid (unrecognised verdict)") ;;
       esac ;;
    *) FAILED=$((FAILED + 1)); FAILED_IDS+=("$jid") ;;
  esac
done
cat "$SANDBOX_HOME"/j-*.jsonl > "$JSONL" 2>/dev/null || true

# Aggregate coverage across the whole lane, read back from the JSONL the runner
# wrote rather than re-counted here — one definition of "executed", in the place
# that decides it. Each journey contributes exactly one kind=journey row.
read -r COV_RAN COV_DECL < <(
  awk -F'"' '/"kind":"journey"/{
      match($0, /"steps_ran":[0-9]+/);  r = substr($0, RSTART+12, RLENGTH-12)
      match($0, /"steps_declared":[0-9]+/); d = substr($0, RSTART+17, RLENGTH-17)
      ran += r; decl += d
    } END {print ran+0, decl+0}' "$JSONL" 2>/dev/null
)
COV_PCT=0
[ "${COV_DECL:-0}" -gt 0 ] && COV_PCT=$(( COV_RAN * 100 / COV_DECL ))

echo
MAN_PCT=0
[ "${MANIFEST_STEPS:-0}" -gt 0 ] && MAN_PCT=$(( COV_RAN * 100 / MANIFEST_STEPS ))

echo "cli-journey-sandbox: $PASSED proved, $PARTIAL partial, $UNATTEMPTED not attempted, $UNPROVEN unproven, $VACUOUS vacuous, $FAILED failed (of ${#JOURNEY_IDS[@]} journeys in the manifest)"
if [ "$UNATTEMPTED" -gt 0 ]; then
  # Name WHY, because the two halves have different owners: skip_live is the
  # author's stated scope, while a lane gap is evidence somebody else has to
  # produce — and the nightly does, read-only, against the operator's daemon.
  echo "                     not attempted: $UNATT_SKIPLIVE declared skip_live, $UNATT_LACKS this lane lacks"
  echo "                     (the lacked set is what cli-journey-nightly.sh runs read-only)"
fi
[ "$PARTIAL" -gt 0 ] && printf '  ~ %s (ran, but its sequence was not proven)\n' "${PARTIAL_IDS[@]}"
echo "                     coverage $COV_RAN/$COV_DECL steps in journeys this lane ENTERED (${COV_PCT}%)"
# The honest denominator. $COV_DECL counts only journeys the lane attempted;
# journeys dropped whole by `skip_live` or --exclude contribute to neither
# side of that ratio, which flatters it by more than half (49% vs 23% on the
# first run reporting both). Quoting the attempted ratio alone is the same
# move as a ✓ on a journey that executed nothing.
echo "                     manifest $COV_RAN/$MANIFEST_STEPS steps in the WHOLE manifest (${MAN_PCT}%)"
[ "$FAILED" -gt 0 ]  && printf '  ✗ %s\n' "${FAILED_IDS[@]}"
if [ "$UNPROVEN" -gt 0 ]; then
  printf '  ⊘ %s (ran end to end and asserted NOTHING)\n' "${UNPROVEN_IDS[@]}"
  echo
  echo "  A ⊘ journey is the one shape worse than a red: it is a sequence of"
  echo "  commands nobody checked the output of, reported as a run. Add an"
  echo "  \`expect\` block to its steps in docs/cli-contract.toml."
fi
if [ "$VACUOUS" -gt 0 ]; then
  printf '  ∅ %s (executed nothing — needs a fixture)\n' "${VACUOUS_IDS[@]}"
  echo
  echo "  A ∅ journey is not a broken feature; it is an untested one. Supply its"
  echo "  fixture (JOURNEY_CORPUS=sep, SOVEREIGN_JOURNEY_* — see cli-journey-verify.sh)"
  echo "  or accept that this lane makes no claim about it."
fi

# Same ordering as the runner: a real failure outranks an absence of evidence.
if [ "$FAILED" -gt 0 ]; then RC=1
elif [ "$VACUOUS" -gt 0 ] || [ "$UNPROVEN" -gt 0 ]; then RC=4
else RC=0
fi
# Keep the sandbox HOME for triage on ANY non-zero verdict, vacuous included —
# the per-journey JSONL is how you find out which fixture was missing.
exit "$RC"

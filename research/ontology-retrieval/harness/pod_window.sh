#!/usr/bin/env bash
# pod_window.sh — one metered window on a rented GPU pod: run a batch of
# commands against it, then destroy it, whatever happens.
#
#   research/ontology-retrieval/harness/pod_window.sh <batch-file>
#   research/ontology-retrieval/harness/pod_window.sh --rehearse [<batch-file>]
#
# THE MONEY IS THE POINT. A Vast instance bills until `dev-pod.sh down`
# destroys it, so teardown hangs off a `trap` that fires on success, on
# failure and on a signal — and then READS `dev-pod.sh status` BACK, because a
# teardown that no-ops on the happy path is the worst bug this script could
# carry (`dev-pod.sh` lost a pod to a missing `-y` on 2026-08-29; its `down`
# arm carries that note). If the status does not say nothing is billing, this
# writes `ralph/NEEDS_HUMAN.md` headed POD STILL BILLING rather than exiting
# quietly with a meter running.
#
# EVERY BATCH LINE IS A PLAIN FOREGROUND CHILD. Never `nohup` one: spike 3 lost
# a build to a detached child whose output went nowhere and whose exit code
# nobody read. One log per line, one wall clock per line, and the run stops at
# the first non-zero exit — on a meter, running the steps that depended on a
# failed one just spends money to fail again.
#
# `--rehearse` is the preflight, and it is why this script exists before the
# rental: the SAME batch file, the SAME argv, against the LOCAL daemon, with
# every `enrich` verb given `--dry-run` and every `eval` capped at one
# question. It never calls `down`, because it never rented anything. Every
# transform is printed in the table — a rehearsal that silently ran something
# other than what the batch says would be the one failure a preflight cannot
# have (ARCH §6).
#
# The batch file is DATA: one shell command per line, `#` comments and blank
# lines skipped, `$SVRN` expanded to this host's CLI and `$POD_WINDOW_OUT` to
# this window's own log directory.

set -uo pipefail

# Repo root without a `../` segment, and without assuming the caller's cwd.
REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && git rev-parse --show-toplevel 2>/dev/null)"
[ -n "$REPO" ] || { echo "pod_window: not inside a git work tree" >&2; exit 2; }
cd "$REPO" || exit 2

DEV_POD="scripts/dev-pod.sh"
DEFAULT_BATCH="research/ontology-retrieval/harness/batch-stage0.txt"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
# The HOME daemon's client port, as `dev-pod.sh` itself fixes it (:94). The POD
# port is never written here: it has exactly one source, the `env` verb's own
# output, which is also what gets exported (ARCH §8).
HOME_URL="http://127.0.0.1:9741"

say() { printf '%s\n' "$*"; }
die() { printf 'pod_window: %s\n' "$*" >&2; exit 2; }
usage() { sed -n '2,33p' "$0"; }

# ── arguments ────────────────────────────────────────────────────────────────

REHEARSE=0
BATCH=""
for a in "$@"; do
    case "$a" in
        --rehearse) REHEARSE=1 ;;
        -h|--help)  usage; exit 0 ;;
        -*)         die "unknown flag $a" ;;
        *)          [ -n "$BATCH" ] && die "one batch file; got a second: $a"
                    BATCH="$a" ;;
    esac
done
BATCH="${BATCH:-$DEFAULT_BATCH}"
[ -f "$BATCH" ] || die "no batch file at $BATCH"

# ── the batch ────────────────────────────────────────────────────────────────

LINES=()
while IFS= read -r ln || [ -n "$ln" ]; do
    case "${ln#"${ln%%[![:space:]]*}"}" in ''|'#'*) continue ;; esac
    LINES+=("$ln")
done < "$BATCH"
# A window that runs nothing is not a clean window, the same way a zero-test
# run is never green (`sovereign-test.sh` exit 4).
[ "${#LINES[@]}" -gt 0 ] || die "$BATCH holds no command lines"

# The CLI under whichever of its two names this host has — the same rule, and
# the same order, as `run_arm.py:253`. Exported so a batch line can say
# `"$SVRN" corpus install …` and mean the binary that is actually here.
SVRN=""
for n in svrn sovereign; do
    if p="$(command -v "$n" 2>/dev/null)"; then SVRN="$p"; break; fi
done
[ -n "$SVRN" ] || die "neither \`svrn\` nor \`sovereign\` is on PATH — \
ln -sf \$(realpath target/debug/sovereign-cli) ~/.local/bin/sovereign"
export SVRN

# ── which daemon, and does it answer ─────────────────────────────────────────

ENV_BLOCK=""
if [ "$REHEARSE" = 1 ]; then
    # Structural, not hoped-for: a shell that has already been pointed at a pod
    # must still rehearse against the machine in front of it (ARCH §10).
    export SOVEREIGN_DAEMON_URL="$HOME_URL"
    export SVRNMESH_DAEMON_URL="$HOME_URL"
    DAEMON="$HOME_URL"
else
    [ -x "$DEV_POD" ] || die "no $DEV_POD"
    ENV_BLOCK="$("$DEV_POD" env)" || die "$DEV_POD env failed"
    # Three `export` lines is the shape this script was written against; a
    # fourth, or a line that is not an export, is a contract change and is
    # refused rather than eval'd.
    n_export=$(printf '%s\n' "$ENV_BLOCK" | grep -c '^export ')
    n_total=$(printf '%s\n' "$ENV_BLOCK" | grep -c .)
    [ "$n_export" = 3 ] && [ "$n_total" = 3 ] \
        || die "$DEV_POD env printed $n_total line(s), $n_export of them exports — expected 3 exports"
    DAEMON="$(printf '%s\n' "$ENV_BLOCK" | sed -n 's/^export SOVEREIGN_DAEMON_URL=//p' | head -1)"
    [ -n "$DAEMON" ] || die "$DEV_POD env printed no SOVEREIGN_DAEMON_URL"
fi

models_json="$(curl -s -m 5 "$DAEMON/v1/models" 2>/dev/null)"
case "$models_json" in
    *'"id"'*) MODEL="$(printf '%s' "$models_json" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)" ;;
    *)  if [ "$REHEARSE" = 1 ]; then
            die "the local daemon at $DAEMON lists no model — start it before rehearsing (this script never starts it)"
        else
            die "nothing answers at $DAEMON — open the tunnel first: $DEV_POD tunnel (and $DEV_POD logs to watch the slot load)"
        fi ;;
esac

# ── teardown, armed before the first line and not before the gate ────────────
#
# Armed HERE and not earlier on purpose: a refusal above means the tunnel never
# answered, and the operator may be mid-boot. Destroying their pod because this
# script declined to start would be the wrong reading of a refusal.

TORN=0
write_needs_human() {
    cat > "$NEEDS_HUMAN" <<NH
# POD STILL BILLING

\`$DEV_POD down\` ran and \`$DEV_POD status\` did NOT report that nothing is
billing. A Vast instance bills until it is destroyed, so this is live spend.

## What ran

    $DEV_POD down
    $DEV_POD status

## What status actually said

\`\`\`
$1
\`\`\`

## What the operator must decide

1. Destroy it by hand and confirm: \`vastai show instances\`, then
   \`vastai destroy instance <id> -y\` (\`scripts/dev-pod.sh:883\` — the \`-y\`
   is load-bearing; without it the prompt aborts and the pod keeps billing).
2. Whether the window's own results under \`research/ontology-retrieval/pod/\`
   are usable, or the window must be re-run.

Then edit or mark the row in ralph/next/ei7-stage0/STATE.md, then
\`rm ralph/STOP ralph/NEEDS_HUMAN.md\`.
NH
}

on_exit() {
    code=$?
    trap - EXIT INT TERM
    if [ "$REHEARSE" = 1 ]; then
        say "[pod_window] rehearsal: nothing was rented, so nothing is destroyed"
        exit "$code"
    fi
    [ "$TORN" = 1 ] && exit "$code"
    TORN=1
    say ""
    say "[pod_window] window over (exit $code) — destroying the pod"
    "$DEV_POD" down || say "[pod_window] down exited non-zero; reading status anyway" >&2
    status="$("$DEV_POD" status 2>&1)"
    printf '%s\n' "$status"
    case "$status" in
        *"nothing billing"*) say "[pod_window] billing stopped" ;;
        *) say "[pod_window] status does not say nothing is billing — wrote $NEEDS_HUMAN" >&2
           write_needs_human "$status"
           code=9 ;;
    esac
    exit "$code"
}
trap on_exit EXIT INT TERM

# ── rehearsal transforms, each one named in the table ────────────────────────
#
# `enrich` first and `eval` only otherwise: `enrich eval` is a real verb pair
# (`enrich_cmd/eval/`), and on a dry run the enrich reading is the one that
# spends nothing. `run-pilot.sh --dry-run` is the rehearsal form that script
# declares for exactly this caller (its own header).

# Both results come back in globals rather than on stdout: a command
# substitution would run this in a subshell, and the transform's NAME would be
# lost on the way back — leaving the table's `rehearsed` column blank while the
# transform really had been applied. That is the one shape a preflight cannot
# have, so the coupling is structural (ARCH §6, §10).
REH_CMD=""
REH_VIA="-"
REH_SKIP=0
rehearse_line() {
    REH_CMD="$1"
    REH_VIA="-"
    REH_SKIP=0
    case "$1" in
        # First arm, and it has to be: the extract parser REFUSES the pair
        # (`sovereign-enrichment-build/src/extract/args.rs:116-127`), so the
        # `enrich` arm below would rehearse this line into a guaranteed exit 2.
        # Dropping the flag instead would rehearse a DIFFERENT command than the
        # batch says, which is the one thing a preflight must never do — so the
        # line is not run at all and the table says so (ARCH §6).
        *--finalize*)   REH_CMD="$1"; REH_VIA="skipped — --finalize refuses --dry-run"; REH_SKIP=1 ;;
        *run-pilot.sh*) REH_CMD="$1 --dry-run"; REH_VIA="dry-run" ;;
        *" enrich "*)   REH_CMD="$1 --dry-run"; REH_VIA="dry-run" ;;
        *" eval "*)     REH_CMD="$1 --limit 1"; REH_VIA="limit-1" ;;
    esac
}

# ── the run ──────────────────────────────────────────────────────────────────

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
[ "$REHEARSE" = 1 ] && STAMP="rehearse-$STAMP"
OUT="research/ontology-retrieval/pod/$STAMP"
mkdir -p "$OUT" || die "cannot create $OUT"
# Exported for the same reason `$SVRN` is: a batch line that has to write into
# THIS window's directory cannot name it, and an unexported `$OUT` would expand
# to nothing in the child and land the copy at `/runs`.
export POD_WINDOW_OUT="$OUT"

say "== pod window"
say "mode     $([ "$REHEARSE" = 1 ] && echo 'rehearse (local daemon, no rental, no teardown)' || echo 'live (rented pod, destroyed on exit)')"
say "batch    $BATCH (${#LINES[@]} lines)"
say "daemon   $DAEMON serving $MODEL"
say "cli      $SVRN"
say "logs     $OUT"
[ "$REHEARSE" = 0 ] && { say "env      $(printf '%s' "$ENV_BLOCK" | tr '\n' ' ')"; eval "$ENV_BLOCK"; }

NN=(); EXITS=(); WALLS=(); CMDS=(); VIAS=()
failed=0
skipped=0
i=0
for raw in "${LINES[@]}"; do
    i=$((i + 1))
    nn="$(printf '%02d' "$i")"
    cmd="$raw"; via="-"
    if [ "$REHEARSE" = 1 ]; then rehearse_line "$raw"; cmd="$REH_CMD"; via="$REH_VIA"; fi

    NN+=("$nn"); CMDS+=("$cmd"); VIAS+=("$via")
    if [ "$REHEARSE" = 1 ] && [ "$REH_SKIP" = 1 ]; then
        # Neither pass nor fail: the tally below counts it apart, so a rehearsal
        # cannot report a line as green that it never ran (ARCH §5).
        EXITS+=("skipped"); WALLS+=("-")
        skipped=$((skipped + 1))
        say ""
        say "-- $nn $via: $cmd"
        continue
    fi
    if [ "$failed" = 1 ]; then
        EXITS+=("skip"); WALLS+=("-")
        say ""
        say "-- $nn skipped (an earlier line failed): $cmd"
        continue
    fi

    say ""
    say "-- $nn $cmd"
    start="$(date +%s)"
    bash -c "$cmd" > "$OUT/$nn.log" 2>&1
    code=$?
    wall=$(( $(date +%s) - start ))
    EXITS+=("$code"); WALLS+=("${wall}s")
    say "   exit=$code  wall=${wall}s  log=$OUT/$nn.log"
    if [ "$code" != 0 ]; then
        failed=1
        say "   last 20 lines:" >&2
        tail -20 "$OUT/$nn.log" | sed 's/^/   | /' >&2
    fi
done

# ── the table ────────────────────────────────────────────────────────────────

{
    printf '%-4s %-8s %-6s %-9s %s\n' "line" "exit" "wall" "rehearsed" "command"
    for j in "${!NN[@]}"; do
        printf '%-4s %-8s %-6s %-9s %s\n' \
            "${NN[$j]}" "${EXITS[$j]}" "${WALLS[$j]}" "${VIAS[$j]}" "${CMDS[$j]}"
    done
} | tee "$OUT/table.txt"

say ""
if [ "$failed" = 1 ]; then
    say "pod_window: batch FAILED — see the table and $OUT/*.log"
    exit 1
fi
ran=$(( ${#LINES[@]} - skipped ))
if [ "$skipped" -gt 0 ]; then
    say "pod_window: $ran/$ran run lines exit 0; $skipped skipped, claiming nothing"
else
    say "pod_window: ${#LINES[@]}/${#LINES[@]} lines exit 0"
fi
exit 0

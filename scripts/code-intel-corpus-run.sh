#!/usr/bin/env bash
# Whole-corpus code-intel enrichment for a SCIP-indexed CODE corpus.
#
# WHY A LAUNCHD ONE-SHOT: this is ~11h of model calls. An agent harness reaps
# the process tree it tracks, so a run started from a session dies with the
# session. launchd owns this instead (see contrib/launchd/, and the sibling
# plist ai.sovereign.enrich.code-intel-<corpus>.plist).
#
# CACHE-ONLY BY DESIGN (SOVEREIGN_ENRICH_SKIP_INDEX=1). The expensive half is
# generation; indexing is minutes. The daemon stays up for the whole run to
# serve inference, and it co-manages chunks.lance for this corpus — writing the
# index from a second process while it does is the documented conflict
# (pass.rs:142). So phase 1 produces only code_intel_cache.json, which is
# checkpointed every 200 symbols and is what makes this resumable. Phase 2
# (index) is a re-run of the same command WITHOUT this env var: every summary
# hits the cache, costs zero model calls, and only embeds + upserts.
#
# THE PREFLIGHT REFUSES RATHER THAN SUBSTITUTES. The cache key is
# {body_hash}/{prompt}v{VERSION} and carries NO model identity, so if the slot
# is serving a different model than intended, the run silently produces 37k
# summaries nothing can attribute — and a later re-run reuses them. Cheaper to
# refuse in the first second than to discover it in the morning (ARCH §18.3).
set -uo pipefail

CORPUS="${1:-commonwealth-ai}"
ROLE="${2:-fast}"
EXPECT_MODEL="${3:-Qwopus3.5-4B}"

REPO=/Users/alexsbryan/dev/commonwealth-ai
BIN="$REPO/target/debug/sovereign-cli-llm"
RUNDIR="$REPO/runs/code-intel-$CORPUS"
LOG="$RUNDIR/run.log"
CURL=/usr/bin/curl
PY=/usr/bin/python3

mkdir -p "$RUNDIR"
say() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" | tee -a "$LOG"; }

say "=== code-intel corpus run: corpus=$CORPUS role=$ROLE expect=$EXPECT_MODEL ==="

[ -x "$BIN" ] || { say "REFUSE: $BIN is not executable — build it first"; exit 2; }

# Preflight 1: daemon reachable.
if ! "$CURL" -sf --max-time 20 http://localhost:9741/status -o "$RUNDIR/.status.json"; then
  say "REFUSE: daemon not answering at http://localhost:9741/status"
  exit 2
fi

# Preflight 2: the slot is serving the model this run claims.
# Read the role's model from the status file. A heredoc and a `< file` redirect
# would both claim stdin; an earlier version did exactly that and python parsed
# the JSON as its own source, so the gate refused unconditionally. A gate that
# always fails is not a gate either. Pass both as argv.
SERVED=$("$PY" -c '
import json,sys
d=json.load(open(sys.argv[1]))
for e in d.get("inference",{}).get("resident",[]):
    if e.get("role")==sys.argv[2]:
        print(e.get("model_id","")); break
' "$RUNDIR/.status.json" "$ROLE")
if [ -z "$SERVED" ]; then
  say "REFUSE: daemon reports no '$ROLE' role"
  exit 3
fi
case "$SERVED" in
  *"$EXPECT_MODEL"*) say "preflight ok: $ROLE slot serves '$SERVED'" ;;
  *) say "REFUSE: $ROLE slot serves '$SERVED', expected to contain '$EXPECT_MODEL'. Not writing 37k summaries I cannot attribute."; exit 3 ;;
esac

# Preflight 3: the corpus config must route chat at the role we just verified.
CFG="$HOME/.svrnmesh/enrichment/$CORPUS/config.json"
ROUTED=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('chat_model',''))" "$CFG" 2>/dev/null)
if [ "$ROUTED" != "$ROLE" ]; then
  say "REFUSE: $CFG routes chat_model='$ROUTED' but this run verified role '$ROLE'"
  exit 3
fi
say "preflight ok: $CORPUS routes chat_model='$ROUTED'"

PRIOR=$("$PY" -c "
import json,sys
try: print(len(json.load(open(sys.argv[1]))))
except Exception: print(0)
" "$HOME/.svrnmesh/indexes/$CORPUS/code_intel_cache.json" 2>/dev/null)
say "cache on entry: $PRIOR summaries (these are reused, not regenerated)"

export SOVEREIGN_ENRICH_SKIP_INDEX=1
export RUST_LOG="${RUST_LOG:-info}"

say "starting generation — checkpoints land every 200 symbols"
START=$(date +%s)
cd "$REPO" || exit 2
"$BIN" enrich code-intel "$CORPUS" >>"$LOG" 2>&1
RC=$?
ELAPSED=$(( $(date +%s) - START ))

FINAL=$("$PY" -c "
import json,sys
try: print(len(json.load(open(sys.argv[1]))))
except Exception: print(0)
" "$HOME/.svrnmesh/indexes/$CORPUS/code_intel_cache.json" 2>/dev/null)

say "exit=$RC elapsed=${ELAPSED}s cache: $PRIOR -> $FINAL summaries (+$((FINAL-PRIOR)))"
if [ "$RC" -ne 0 ]; then
  say "NONZERO EXIT — the cache is intact and checkpointed; relaunch resumes from $FINAL"
fi
say "=== done ==="
exit $RC

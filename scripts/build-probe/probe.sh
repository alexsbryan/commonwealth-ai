#!/usr/bin/env bash
# Incremental-build probe: touch one hot file, time the dev-loop shapes, restore.
set -u
cd "$(git rev-parse --show-toplevel)"
S="${BUILD_PROBE_OUT:-$PWD/target/build-probe}"; mkdir -p "$S/logs" "$S/timings"
LOCK=scripts/with-cargo-lock.sh
GATE="corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-turn-client/bundled-backend"
BUILDF="corpus-engine/treesitter,sovereign-cli/dev-tools"
RES=$S/results.tsv
echo -e "probe\tcrate\tshape\trun\twall_s\texit\tnote" > $RES
TOUCHED=""
restore() { [[ -n "$TOUCHED" ]] && git checkout -- "$TOUCHED" 2>/dev/null; TOUCHED=""; }
trap 'restore; echo "ABORTED $(date)" >> $RES' INT TERM
N=0
touch_file() { TOUCHED="$1"; N=$((N+1)); python3 - "$1" $N <<'PY'
import re,sys
f,n=sys.argv[1],int(sys.argv[2])
lines=open(f).read().split('\n')
pat=re.compile(r'^\s*(pub(\([^)]*\))?\s+)?(async\s+)?(unsafe\s+)?fn\s+[a-z_0-9]+[^;]*\{\s*$')
for i,l in enumerate(lines):
    if pat.match(l):
        ind=re.match(r'\s*',l).group(0)+'    '
        lines.insert(i+1,f'{ind}let __build_probe_{n}: u32 = {n}; let _ = __build_probe_{n};')
        break
else:
    lines.append(f'// build-probe touch {n}')
open(f,'w').write('\n'.join(lines))
PY
}
timeit() { # label crate shape run cmd...
  local probe=$1 crate=$2 shape=$3 run=$4; shift 4
  local t0=$(date +%s.%N)
  "$@" > $S/logs/${probe//\//_}.$shape.$run.log 2>&1
  local rc=$?
  local t1=$(date +%s.%N)
  local w=$(python3 -c "print(round($t1-$t0,1))")
  local note=""
  if [[ -f target/cargo-timings/cargo-timing.html && "$shape" != lint-scoped && "$shape" != test ]]; then
    cp target/cargo-timings/cargo-timing.html $S/timings/${probe//\//_}.$shape.$run.html
    rm -f target/cargo-timings/cargo-timing.html
  fi
  echo -e "$probe\t$crate\t$shape\t$run\t$w\t$rc\t$note" >> $RES
  echo "[$(date +%H:%M:%S)] $probe $shape run=$run wall=${w}s rc=$rc"
}
mkdir -p $S/logs
echo "=== warm-ups $(date) ==="
timeit warm - check-full 1 $LOCK cargo check --workspace --all-targets --features "$GATE" --timings
timeit warm - build-ws 1 $LOCK cargo build --workspace --features "$BUILDF" --timings

# file|crate|test
PROBES=(
"sovereign/crates/sovereign-mesh/src/daemon.rs|sovereign-mesh|partition_round_robin_balances"
"sovereign/crates/sovereign-core/src/deep_research/mod.rs|sovereign-core|gates_require_both_absolute_and_margin"
"corpus-engine/src/engine/mod.rs|corpus-engine|stamp_then_load_round_trips"
"sovereign/crates/sovereign-cli-llm/src/lib.rs|sovereign-cli-llm|shard_index_and_count_read_the_convention"
"sovereign/crates/sovereign-desktop/src-tauri/src/state.rs|sovereign-desktop|deterministic_with_seed"
"sovereign/crates/sovereign-tools/src/local_corpus/manager.rs|sovereign-tools|is_pathological_all_zero"
"sovereign/crates/sovereign-contracts/src/setup_config.rs|sovereign-contracts|noop_observer_is_send_sync"
"sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs|sovereign-cli-daemon|restarts_on_panic_then_completes"
"sovereign/crates/sovereign-turn-client/src/lib.rs|sovereign-turn-client|the_default_ready_path_is_v1_models"
"kernel-types/src/lib.rs|kernel-types|hex_round_trips"
"sovereign/crates/sovereign-core/tests/main/f26_egress_census.rs|sovereign-core|gates_require_both_absolute_and_margin"
)
for p in "${PROBES[@]}"; do
  IFS='|' read -r f crate test <<< "$p"
  echo "=== probe $f ($crate) $(date) ==="
  if [[ "$f" != *"/tests/"* ]]; then
    for run in 0 1; do touch_file "$f"; timeit "$f" $crate lint-scoped $run $LOCK ./scripts/sovereign-lint.sh --human; done
    touch_file "$f"; timeit "$f" $crate check-full 1 $LOCK cargo check --workspace --all-targets --features "$GATE" --timings
    touch_file "$f"; timeit "$f" $crate build-ws 1 $LOCK cargo build --workspace --features "$BUILDF" --timings
  fi
  for run in 0 1; do touch_file "$f"; timeit "$f" $crate test $run $LOCK ./scripts/sovereign-test.sh --human --package $crate --filter $test; done
  restore
done
echo "=== done $(date) ==="; git status --short | head

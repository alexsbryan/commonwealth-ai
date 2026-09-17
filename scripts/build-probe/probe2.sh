#!/usr/bin/env bash
# Follow-up: (A) fingerprint-log experiment for the scoped-lint -> full-check flip;
# (B) clean check-full / build-ws for crates whose first pass was contaminated by the previous probe's restore.
set -u
cd "$(git rev-parse --show-toplevel)"
S="${BUILD_PROBE_OUT:-$PWD/target/build-probe}"; mkdir -p "$S/logs" "$S/timings"
LOCK=scripts/with-cargo-lock.sh
GATE="corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-turn-client/bundled-backend"
BUILDF="corpus-engine/treesitter,sovereign-cli/dev-tools"
RES=$S/results2.tsv
echo -e "probe\tcrate\tshape\trun\twall_s\texit\tnote" > $RES
TOUCHED=""
restore() { [[ -n "$TOUCHED" ]] && git checkout -- "$TOUCHED" 2>/dev/null; TOUCHED=""; }
trap 'restore; echo "ABORTED $(date)" >> $RES' INT TERM
N=100
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
open(f,'w').write('\n'.join(lines))
PY
}
timeit() { local probe=$1 crate=$2 shape=$3 run=$4; shift 4
  local t0=$(date +%s.%N); "$@" > $S/logs/${probe//\//_}.$shape.$run.log 2>&1; local rc=$?; local t1=$(date +%s.%N)
  local w=$(python3 -c "print(round($t1-$t0,1))")
  if [[ -f target/cargo-timings/cargo-timing.html && "$shape" != lint-scoped && "$shape" != test ]]; then cp target/cargo-timings/cargo-timing.html $S/timings/${probe//\//_}.$shape.$run.html; rm -f target/cargo-timings/cargo-timing.html; fi
  echo -e "$probe\t$crate\t$shape\t$run\t$w\t$rc\t" >> $RES; echo "[$(date +%H:%M:%S)] $probe $shape run=$run wall=${w}s rc=$rc"; }
settle() { $LOCK cargo check --workspace --all-targets --features "$GATE" >/dev/null 2>&1; $LOCK cargo build --workspace --features "$BUILDF" >/dev/null 2>&1; }
echo "=== settle $(date) ==="; settle
echo "=== (A) fingerprint experiment $(date) ==="
# A1: no-op full check (expect ~7s)
timeit flip - check-full noop $LOCK cargo check --workspace --all-targets --features "$GATE" --timings
# A2: scoped lint with NO source change: does it rebuild anything?
timeit flip - lint-scoped-noedit 0 $LOCK env CARGO_LOG=cargo::core::compiler::fingerprint=info cargo check -p sovereign-mesh -p sovereign-cli-daemon -p sovereign-cli-dev -p sovereign-cli-llm --all-targets --features corpus-engine/treesitter,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-mesh/treesitter
# A3: full check again with NO source change: does it rebuild anything after the scoped one?
timeit flip - check-full-after-scoped 0 $LOCK env CARGO_LOG=cargo::core::compiler::fingerprint=info cargo check --workspace --all-targets --features "$GATE" --timings
# A4: scoped again (second flip)
timeit flip - lint-scoped-noedit 1 $LOCK env CARGO_LOG=cargo::core::compiler::fingerprint=info cargo check -p sovereign-mesh -p sovereign-cli-daemon -p sovereign-cli-dev -p sovereign-cli-llm --all-targets --features corpus-engine/treesitter,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-mesh/treesitter
echo "=== (B) clean re-measure $(date) ==="; settle
PROBES=(
"sovereign/crates/sovereign-cli-llm/src/lib.rs|sovereign-cli-llm"
"sovereign/crates/sovereign-desktop/src-tauri/src/state.rs|sovereign-desktop"
"sovereign/crates/sovereign-tools/src/local_corpus/manager.rs|sovereign-tools"
"sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs|sovereign-cli-daemon"
"sovereign/crates/sovereign-turn-client/src/lib.rs|sovereign-turn-client"
"sovereign/crates/sovereign-mesh/src/daemon.rs|sovereign-mesh"
"sovereign/crates/sovereign-contracts/src/setup_config.rs|sovereign-contracts"
)
for p in "${PROBES[@]}"; do IFS='|' read -r f crate <<< "$p"
  echo "=== probe2 $f ($crate) $(date) ==="
  touch_file "$f"; timeit "$f" $crate check-full 2 $LOCK cargo check --workspace --all-targets --features "$GATE" --timings
  touch_file "$f"; timeit "$f" $crate build-ws 2 $LOCK cargo build --workspace --features "$BUILDF" --timings
  restore; settle
done
echo "=== done $(date) ==="; git status --short | head

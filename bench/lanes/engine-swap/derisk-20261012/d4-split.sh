#!/bin/sh
# D4: three placements of ONE hybrid model across the Halo (head, Vulkan) and
# the Mac (peer, Metal ggml-rpc-server), over the LAN. The question the row
# asks is whether OUR byte-aware planner earns a place over upstream's split,
# so (a) and (b) are upstream's two shapes and (c) is the planner's.
#
#   a  upstream's default: no -ts, llama.cpp divides by the free memory each
#      device advertises (the peer's over RPC, ours from the shipped binary)
#   b  a hand -ts by each device's memory
#   c  the planner's byte-aware -ot: block ranges enforced at load, each
#      buffer named RPC<index within that server>[host:port] (the peer's
#      server-scoped index, NOT the global -dev RPC<n> number)
#
# Usage: d4-split.sh <out-dir> <peer-host:port> [n-runs] [models...]
# Each model gets the three placements, n runs each, 128 tokens at
# temperature 0. A placement that fails to load is recorded as a failure and
# the run continues: "fails to load where (c) succeeds" is one of the two
# verdicts the row names.
set -u

OUT=${1:?out dir}
PEER=${2:?peer host:port}
N=${3:-3}
shift 3

HERE=$(dirname "$0")
ROOT=/home/alexbryan/dev/commonwealth-ai
B=$ROOT/target/debug
MODELS_DIR=${SOVEREIGN_MODELS_DIR:-$ROOT/sovereign/models}
PORT=${D4_PORT:-48430}
CACHE=$OUT/cache
mkdir -p "$OUT" "$CACHE"

# One prompt for every arm, so the comparison is the placement and not the
# question. ignore_eos holds the decode at 128 tokens.
BODY='{"prompt":"Write a detailed history of the printing press.","n_predict":128,"temperature":0,"ignore_eos":true}'

# The Mac's ggml-rpc-server advertises TWO devices: RPC0 with the GPU's memory
# and RPC1 with 0 bytes (Metal emulates virtual devices; the second one here
# reports nothing, and `--fit` says so — "device RPC1 did not report memory").
# Left in the list, llama.cpp's own split hands RPC1 3.4 GiB of model, and the
# first decode reads a tensor from a device holding none: ggml-rpc.cpp:522,
# "Remote RPC server crashed", and on this link the peer's process dies with
# it. So every arm names the real devices explicitly. This is a property of the
# PEER, not of any placement: the row's three arms differ in how the model is
# divided between the two REAL devices, and this line holds that fixed.
DEVS=${D4_DEVS:-RPC0,Vulkan0}

peer_free_gib() {
    python3 "$HERE/d4_peer.py" "$PEER" | sed -n 's/^free_gib //p'
}
my_free_gib() {
    "$B/llama-server" --list-devices 2>/dev/null \
        | sed -n 's/.*(\([0-9]*\) MiB, \([0-9]*\) MiB free).*/\2/p' | head -1 \
        | awk '{printf "%.1f", $1/1024}'
}

wait_health() { # pid seconds: 0 when the server answers, 1 when it dies or times out
    i=0
    while [ "$i" -lt "$2" ]; do
        curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && return 0
        kill -0 "$1" 2>/dev/null || return 1
        i=$((i + 1))
        sleep 0.5
    done
    return 1
}

stop_server() {
    [ -n "${srv:-}" ] && { kill -9 "$srv" 2>/dev/null; wait "$srv" 2>/dev/null; srv=; }
}
trap stop_server EXIT INT TERM

run_arm() { # model arm args...
    model=$1; arm=$2; shift 2
    label="$name/$arm"
    ok=1
    t0=$(date +%s.%N)
    LLAMA_CACHE=$CACHE "$B/llama-server" -m "$model" --port "$PORT" -c 4096 -np 1 \
        -ngl 99 "$@" -dev "$DEVS" >"$OUT/$label.log" 2>&1 &
    srv=$!
    if ! wait_health "$srv" 3600; then
        ok=0
        why=$(tail -3 "$OUT/$label.log" | tr '\n' ' ' | cut -c1-200)
        printf '%s  %s  load=FAIL  %s\n' "$name" "$arm" "$why" | tee -a "$OUT/table.txt"
        stop_server
        return
    fi
    # No `bc` on this host (the shed row's other scripts use it; a fresh
    # Fedora toolbox has python3 and not bc): the subtraction is one line
    # of awk, which is present wherever sh is.
    load=$(awk -v a="$(date +%s.%N)" -v b="$t0" 'BEGIN{printf "%.1f", a-b}')
    rates=""
    failed=0
    for k in $(seq 1 "$N"); do
        r=$(curl -s -m 300 "http://127.0.0.1:$PORT/completion" -d "$BODY" \
            | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print('ERR'); raise SystemExit
t = d.get('timings') or {}
n = t.get('predicted_n') or 0
print(round(t['predicted_per_second'], 2) if n >= 128 else 'ERR')
" 2>/dev/null)
        # A request that answers nothing, or answers with fewer than the 128
        # tokens the row asks for, is a FAILED decode and is named as one. The
        # first version of this loop scored it 0.0 tok/s, which is the
        # success-shaped value ARCH 6 forbids: a dead peer would have entered
        # the table as "slow".
        case "$r" in
            ''|ERR) failed=$((failed + 1)); rates="$rates ERR" ;;
            *) rates="$rates $r" ;;
        esac
        sleep 1
    done
    if [ "$failed" -gt 0 ]; then
        printf '%s  %s  load=%ss  decode=FAILED(%d/%d)  [%s]\n' \
            "$name" "$arm" "$load" "$failed" "$N" "$(echo $rates | sed 's/^ //')" \
            | tee -a "$OUT/table.txt"
        stop_server
        return
    fi
    mean=$(echo $rates | awk '{s=0; for(i=1;i<=NF;i++) s+=$i; if(NF) printf "%.2f", s/NF}')
    printf '%s  %s  load=%ss  decode=%s  mean=%s\n' "$name" "$arm" "$load" "$(echo $rates | sed 's/ /,/g; s/^,//')" "$mean" | tee -a "$OUT/table.txt"
    stop_server
}

pf=$(peer_free_gib)
if [ -z "$pf" ]; then
    # A peer that will not answer is could-not-judge, never a zero: the row is
    # explicit that a run the link will not let finish is retried, never
    # decided by default. Stop BEFORE loading anything, so a retry costs a
    # probe rather than three 30-minute aborts.
    echo "D4 could-not-judge: $PEER does not answer over ggml-rpc (see d4_peer.py)" | tee -a "$OUT/table.txt"
    python3 "$HERE/d4_peer.py" "$PEER" 2>&1 | sed 's/^/  /' | tee -a "$OUT/table.txt"
    exit 4
fi
mf=$(my_free_gib)
echo "peer $PEER free ${pf} GiB, head free ${mf} GiB" | tee "$OUT/table.txt"
# The hand -ts is by memory, as the row says. Rounded to 2dp so the log shows
# the actual ratio the placement asked for.
ts=$(echo "$pf $mf" | awk '{t=$1+$2; printf "%.2f,%.2f", $1/t, $2/t}')
echo "hand -ts $ts (peer,head by advertised free memory)" | tee -a "$OUT/table.txt"

# The planner's cut: 33 blocks of Qwen3.5-9B, apportioned to the same memory.
# Block counts from plan_shards_weighted's count arm (largest remainder) —
# `d4_plan.py` prints the same numbers the crate's planner would, so the -ot
# below is the plan and not a hand guess.
if [ $# -eq 0 ]; then
    set -- "$MODELS_DIR/Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf"
fi
for model in "$@"; do
    name=$(basename "$model" .gguf)
    mkdir -p "$OUT/$name"
    layers=$(python3 "$HERE/d4_plan.py" layers "$model" 2>/dev/null || echo 33)
    python3 "$HERE/d4_plan.py" mass "$model" | sed 's/^/   /' | tee -a "$OUT/table.txt"
    cut=$(python3 "$HERE/d4_plan.py" plan "$model" "$pf" "$mf")
    peer_blocks=$(echo "$cut" | cut -d' ' -f1)
    ot=$(python3 "$HERE/d4_plan.py" ot "$peer_blocks" "$layers" "$PEER")
    dts=$(python3 "$HERE/d4_plan.py" dts "$peer_blocks" "$layers")
    echo "-- $name: $layers blocks, planner gives peer $peer_blocks, head the rest" | tee -a "$OUT/table.txt"
    echo "   planner -ot $ot" | tee -a "$OUT/table.txt"
    echo "   planner -ts $dts (its dev_layer_tensor_split, NOT the memory ratio)" | tee -a "$OUT/table.txt"

    # Order is not fixed: every arm of every round, the round order rotated,
    # so a warm page cache or a warming link cannot land on one arm alone. A
    # single a,b,c sequence let the first arm of a sweep keep an advantage
    # (run3: a took all three of its quickest readings first).
    arm_a() { run_arm "$model" a-default --rpc "$PEER"; }
    arm_b() { run_arm "$model" b-ts --rpc "$PEER" --tensor-split "$ts"; }
    # c is the planner as the crate ships it: -ot places the weights and its
    # dev_layer_tensor_split keeps llama.cpp's own per-layer map on the same
    # cut. Without the -ts, llama.cpp's map stays on upstream's split and the
    # two disagree by a layer — the hybrid-GDN hazard plan.rs:583-606 names.
    arm_c() { run_arm "$model" c-planner --rpc "$PEER" -ot "$ot" --tensor-split "$dts"; }
    rounds=${D4_ROUNDS:-3}
    # `run_arm` measures into `r`; the round counter is its own name so the
    # two cannot collide (they did: the second round read "13.36" as a count).
    round_i=0
    while [ "$round_i" -lt "$rounds" ]; do
        case $((round_i % 3)) in
            0) arm_a; arm_b; arm_c ;;
            1) arm_b; arm_c; arm_a ;;
            2) arm_c; arm_a; arm_b ;;
        esac
        round_i=$((round_i + 1))
    done
done
echo "table: $OUT/table.txt"

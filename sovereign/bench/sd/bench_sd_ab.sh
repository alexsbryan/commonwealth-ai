#!/usr/bin/env bash
# Spike 2 Phase 0 — llama-server SD A/B harness.
#
# Stands up llama-server in three configurations (baseline / n-gram /
# classic-draft) against Qwen3.6-35B-A3B-UD-Q4_K_XL on the local Strix
# Halo (ROCm). For each config, sends N pipeline-Drafter-shaped prompts
# via HTTP, captures llama-server's per-request `timings` payload
# (prompt_n, predicted_n, prompt_per_second, predicted_per_second,
# predicted_ms). Reports per-config averages so SD's actual wall-time
# impact on THIS hardware + THIS model class is measured before any
# integration design is committed to.
#
# Usage:
#   ./bench_sd_ab.sh baseline                       # one config
#   ./bench_sd_ab.sh ngram-cache                    # one config
#   ./bench_sd_ab.sh classic                        # one config (loads draft)
#   ./bench_sd_ab.sh all                            # all three sequentially
#
# Output: writes per-prompt JSON lines to bench/sd/results_<config>.jsonl
# and a one-line summary to stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SERVER_BIN="${SERVER_BIN:-/usr/local/bin/llama-server}"
TARGET_MODEL="${TARGET_MODEL:-$REPO_ROOT/sovereign/models/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf}"
DRAFT_MODEL="${DRAFT_MODEL:-$REPO_ROOT/sovereign/models/Qwen3.5-0.8B-UD-Q6_K_XL.gguf}"
PORT="${PORT:-8765}"
HOST="${HOST:-127.0.0.1}"
N_CTX="${N_CTX:-8192}"
N_PREDICT="${N_PREDICT:-300}"   # cap decode per request — keeps the run bounded
DRAFT_MAX="${DRAFT_MAX:-5}"     # n_draft when SD is enabled
LOG_DIR="$REPO_ROOT/sovereign/bench/sd"
mkdir -p "$LOG_DIR"

# Drafter-shaped pipeline prompts. Shape mirrors the package wrapper
# emitted by pipeline/runner.rs:530. Five prompts at 4 intents to cover
# the typical pipeline distribution. Each prompt is short so the run
# completes quickly; per-decode tok/s is what we measure, not absolute
# wall time.
declare -a PROMPTS=(
"You are answering a knowledge query from a vector-retrieved package. Synthesize, do not infer.

<package>
[chunk 1] The Battle of Midway, fought June 4-7 1942, marked a strategic turning point in the Pacific theatre of World War II. The United States Navy decisively defeated an attacking fleet of the Imperial Japanese Navy near Midway Atoll.
[chunk 2] Japanese carrier losses at Midway included Akagi, Kaga, Soryu, and Hiryu — four of the six fleet carriers that had attacked Pearl Harbor six months earlier.
[chunk 3] Cryptographic intelligence allowed the U.S. to ambush the Japanese strike force. Code-breakers had partially deciphered the JN-25 naval code by May 1942.
</package>

Write a 3-paragraph synthesis explaining why Midway is considered a turning point."

"You are answering a reasoning query from a vector-retrieved package. Synthesize, do not infer.

<package>
[chunk 1] Plate tectonics describes the large-scale motion of seven large plates and the movements of a larger number of smaller plates of Earth's lithosphere.
[chunk 2] Continental drift was first proposed by Alfred Wegener in 1912. He was unable to identify a mechanism for the movement and his hypothesis was rejected by most geologists of his time.
[chunk 3] Most mountain ranges form at convergent plate boundaries through orogeny — the Andes (Nazca/South American), the Himalayas (Indian/Eurasian), the Rockies (multiple subducting plates over time).
</package>

Explain how the theory of plate tectonics accounts for the distribution of mountain ranges."

"You are answering a comparison query from a vector-retrieved package. Bounded contrast.

<package>
[chunk 1] TCP provides reliable, ordered, error-checked delivery of a stream of bytes between applications running on hosts communicating over an IP network. Connection-oriented.
[chunk 2] UDP is a connectionless datagram protocol. It provides minimal services: no handshake, no acknowledgement, no flow control, no congestion control.
[chunk 3] TCP achieves reliability through sequence numbers, acknowledgements, retransmissions, and a sliding-window flow-control mechanism.
</package>

Compare TCP and UDP delivery guarantees."

"You are answering a knowledge query from a vector-retrieved package. Synthesize, do not infer.

<package>
[chunk 1] The Eiffel Tower was constructed between 1887 and 1889 for the Exposition Universelle, a world's fair held in Paris to mark the centennial of the French Revolution.
[chunk 2] Gustave Eiffel's engineering firm designed and built the tower. Construction took two years, two months, and five days — completed in record time for the period.
[chunk 3] The tower stands 330 metres tall and was the world's tallest man-made structure for 41 years until the Chrysler Building in 1930.
</package>

What year was the Eiffel Tower built?"

"You are answering a reasoning query from a vector-retrieved package. Synthesize, do not infer.

<package>
[chunk 1] The 2008 financial crisis was precipitated by the collapse of the US housing bubble. Housing prices peaked in 2006 and began declining sharply in 2007.
[chunk 2] Subprime mortgage lending, where loans were extended to borrowers with poor credit, expanded dramatically in the early 2000s. Many such loans had adjustable rates that reset to higher payments.
[chunk 3] Complex derivative instruments — collateralized debt obligations and credit default swaps — amplified the impact of mortgage defaults across the global financial system.
[chunk 4] The failure of Lehman Brothers in September 2008 triggered a global credit freeze and emergency interventions from central banks worldwide.
</package>

What were the main causes of the 2008 financial crisis?"
)

start_server() {
    local config="$1"
    local extra_args=()
    case "$config" in
        baseline)
            ;;
        ngram-cache)
            extra_args=(--spec-type ngram-cache --draft-max "$DRAFT_MAX")
            ;;
        ngram-mod)
            extra_args=(--spec-type ngram-mod --draft-max "$DRAFT_MAX")
            ;;
        classic)
            extra_args=(-md "$DRAFT_MODEL" -ngld 999 --draft-max "$DRAFT_MAX")
            ;;
        *)
            echo "unknown config: $config" >&2
            exit 2
            ;;
    esac
    echo "[bench_sd_ab] starting server: config=$config" >&2
    echo "[bench_sd_ab]   target=$TARGET_MODEL" >&2
    if [[ ! -f "$TARGET_MODEL" ]]; then
        echo "[bench_sd_ab] FATAL: target model not found at $TARGET_MODEL" >&2
        exit 2
    fi
    if [[ "$config" == "classic" && ! -f "$DRAFT_MODEL" ]]; then
        echo "[bench_sd_ab] FATAL: draft model not found at $DRAFT_MODEL" >&2
        exit 2
    fi
    local logfile="$LOG_DIR/server_${config}.log"
    nohup "$SERVER_BIN" \
        -m "$TARGET_MODEL" \
        -ngl 999 \
        --host "$HOST" --port "$PORT" \
        -c "$N_CTX" \
        --jinja \
        "${extra_args[@]}" \
        > "$logfile" 2>&1 &
    local pid=$!
    echo "$pid"
}

wait_for_health() {
    local timeout="${1:-180}"
    local start=$SECONDS
    while (( SECONDS - start < timeout )); do
        if curl -sf "http://$HOST:$PORT/health" >/dev/null 2>&1; then
            echo "[bench_sd_ab] server ready after $((SECONDS - start))s" >&2
            return 0
        fi
        sleep 1
    done
    echo "[bench_sd_ab] server failed to become ready within ${timeout}s" >&2
    return 1
}

stop_server() {
    local pid="$1"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid"
        # wait for it to actually exit so VRAM is freed before next config
        local wait_start=$SECONDS
        while kill -0 "$pid" 2>/dev/null && (( SECONDS - wait_start < 30 )); do
            sleep 1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
        fi
    fi
}

run_prompts() {
    local config="$1"
    local out="$LOG_DIR/results_${config}.jsonl"
    : > "$out"
    local total_predicted_per_sec=0
    local total_predicted_ms=0
    local total_predicted_n=0
    local n=0
    for prompt in "${PROMPTS[@]}"; do
        n=$((n+1))
        echo "[bench_sd_ab] prompt $n/${#PROMPTS[@]} (config=$config)" >&2
        local req
        req=$(jq -n \
            --arg p "$prompt" \
            --argjson n_predict "$N_PREDICT" \
            '{prompt: $p, n_predict: $n_predict, temperature: 0.0, cache_prompt: false}')
        local resp
        resp=$(curl -sS \
            -H 'Content-Type: application/json' \
            -d "$req" \
            "http://$HOST:$PORT/completion") || {
            echo "  curl failed" >&2
            continue
        }
        # llama-server emits `timings: {prompt_n, prompt_ms, prompt_per_second,
        # predicted_n, predicted_ms, predicted_per_second, ...}` on /completion.
        local row
        row=$(echo "$resp" | jq -c --arg cfg "$config" --argjson n "$n" \
            '{config: $cfg, prompt_idx: $n, timings: .timings}')
        echo "$row" >> "$out"
        local pps pms pn
        pps=$(echo "$resp" | jq -r '.timings.predicted_per_second')
        pms=$(echo "$resp" | jq -r '.timings.predicted_ms')
        pn=$(echo "$resp"  | jq -r '.timings.predicted_n')
        printf '    timings: predicted_n=%s predicted_ms=%s predicted_per_second=%s\n' "$pn" "$pms" "$pps" >&2
        total_predicted_per_sec=$(awk -v a="$total_predicted_per_sec" -v b="$pps" 'BEGIN{print a+b}')
        total_predicted_ms=$(awk -v a="$total_predicted_ms" -v b="$pms" 'BEGIN{print a+b}')
        total_predicted_n=$(awk -v a="$total_predicted_n" -v b="$pn" 'BEGIN{print a+b}')
    done
    local avg_pps
    avg_pps=$(awk -v t="$total_predicted_per_sec" -v n="$n" 'BEGIN{if (n>0) print t/n; else print 0}')
    local total_decode_s
    total_decode_s=$(awk -v t="$total_predicted_ms" 'BEGIN{print t/1000}')
    printf 'SUMMARY config=%s n=%s total_predicted_n=%s total_decode_s=%.2f avg_predicted_per_sec=%.2f\n' \
        "$config" "$n" "$total_predicted_n" "$total_decode_s" "$avg_pps"
}

run_config() {
    local config="$1"
    local pid
    pid=$(start_server "$config")
    trap "stop_server $pid" EXIT INT TERM
    if ! wait_for_health 240; then
        echo "[bench_sd_ab] config=$config FAILED to start" >&2
        stop_server "$pid"
        trap - EXIT INT TERM
        return 1
    fi
    run_prompts "$config"
    stop_server "$pid"
    trap - EXIT INT TERM
}

main() {
    local cmd="${1:-all}"
    if ! command -v jq >/dev/null 2>&1; then
        echo "jq required" >&2
        exit 2
    fi
    case "$cmd" in
        baseline|ngram-cache|ngram-mod|classic)
            run_config "$cmd"
            ;;
        all)
            run_config baseline
            run_config ngram-cache
            run_config classic
            ;;
        *)
            echo "usage: $0 [baseline|ngram-cache|ngram-mod|classic|all]" >&2
            exit 2
            ;;
    esac
}

main "$@"

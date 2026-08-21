#!/bin/sh
# drb1 flight-replay runner — the admission + fetch stages over the
# logged t7a flight (orders drb1-t1/drb1-t2, campaign drb1-race).
# Rebuilds the driver (debug, toolbox-native) and replays every task's
# recorded rounds through the production admission and fetch code
# paths. Zero web, zero API.
#
# Usage: sh research/deep-research/arms/replay/run.sh [flight-root] [out-dir] [stage]
#   flight-root defaults to the logged t7a std flight
#   out-dir    defaults to this directory
#   stage      admission (drb1-t1) or fetch (drb1-t2: admission+fetch)
set -e
root=$(git rev-parse --show-toplevel)
flight=${1:-"$root/research/deep-research/arms/runs-t7a/std"}
out=${2:-"$root/research/deep-research/arms/replay"}
stage=${3:-fetch}
toolbox run -c sovereign-vulkan cargo build -p sovereign-core --example replay_flight
exec toolbox run -c sovereign-vulkan "$root/target/debug/examples/replay_flight" "$flight" "$out" --stage="$stage"

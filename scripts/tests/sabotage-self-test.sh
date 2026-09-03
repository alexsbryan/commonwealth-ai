#!/usr/bin/env bash
# The mutation harness's own falsifiers, wired so they actually run.
#
# `scripts/sabotage.py --self-test` is a real gate — it exits 1 on a failed
# check — and until 2026-09-03 NOTHING invoked it. It was named in
# `quality/conformance-specs.toml` as a landed destination and otherwise fired
# only when somebody typed it by hand. That is the failure
# `scripts/tests/run-all.sh`'s own header warns about: an opt-in guard decays
# into decoration.
#
# This wrapper exists so run-all.sh's `scripts/tests/*.sh` glob picks it up,
# and so pre-push's trigger (which matches `^scripts/tests/`, plus
# `^scripts/sabotage\.py$` added alongside this file) runs it whenever the
# harness itself changes. No cargo, no network, no models — under a second.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1
exec python3 scripts/sabotage.py --self-test

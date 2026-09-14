#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# heaptrack-daemon.sh — run the daemon in the foreground under heaptrack, with
# the environment the user service would have given it.
#
# Why foreground and not `heaptrack -p`: heaptrack's own help calls runtime
# attach UNSTABLE, and a daemon crash mid-profile is indistinguishable from the
# OOM this exists to explain.
#
# Why the env is parsed and not written here: the service's env comes from the
# `env VAR=...` prefix of the EFFECTIVE ExecStart (the last `ExecStart=/` across
# the unit and its drop-ins, in systemd's order) — unit `Environment=` never
# crosses `toolbox run`. Parsing that line keeps this launch identical to the
# service's without a second copy of the list to drift.
#
# Run INSIDE the sovereign-vulkan toolbox, with the service daemon stopped:
#   toolbox run -c sovereign-vulkan research/deep-research/arms/mem-forensics/heaptrack-daemon.sh <outdir>
set -euo pipefail
OUTDIR=${1:?usage: heaptrack-daemon.sh <outdir>}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
UNIT=$HOME/.config/systemd/user/sovereign.service

[ -f /run/.containerenv ] || { echo "refusing: not inside the toolbox (native builds and GPU both need it)" >&2; exit 2; }
command -v heaptrack >/dev/null || { echo "refusing: heaptrack not installed in this container" >&2; exit 2; }

mapfile -t ENVS < <(python3 - "$UNIT" <<'PY'
import glob, os, shlex, sys
unit = sys.argv[1]
files = [unit] + sorted(glob.glob(unit + ".d/*.conf"))
lines = [l.strip() for f in files if os.path.exists(f) for l in open(f)
         if l.startswith("ExecStart=/")]
if not lines:
    sys.exit("no ExecStart=/ line in " + " ".join(files))
argv = shlex.split(lines[-1][len("ExecStart="):])
i = argv.index("env") + 1
while i < len(argv) and "=" in argv[i] and not argv[i].startswith("/"):
    print(argv[i])
    i += 1
PY
)

mkdir -p "$OUTDIR"
{
  echo "started_at=$(date -Is)"
  echo "head=$(git -C "$REPO" rev-parse --short HEAD)"
  echo "binary_mtime=$(stat -c %y "$REPO/target/debug/sovereign-cli-daemon")"
  printf 'env=%s\n' "${ENVS[@]}"
} > "$OUTDIR/launch.txt"

cd "$REPO"
exec env "${ENVS[@]}" heaptrack --record-only -o "$OUTDIR/heaptrack.daemon" \
  "$REPO/target/debug/sovereign-cli-daemon" daemon run

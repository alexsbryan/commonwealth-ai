#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-work-lift.sh — the work-peer lift, kept under its old name for its callers
# (quality/instruments.toml `cw-work-lift`, commonwealth-work's docs). It is
# `scripts/program-lift.sh --sandbox cw-work`; the closure rule and the RUN
# smoke live in scripts/program-lift.toml.
#
#   scripts/cw-work-lift.sh --sandbox [--image <ref>] [--dir <path>] [--keep]
#
# The RUN step needs a container, so on the Halo it runs on the HOST, where
# podman is. The last stdout line is program-lift's verdict line
# (scripts/lib/judgement.py), which also carries {"value": 1|0}. A measured
# failure exits 0 here, as it always did, because co-lineage.py `measure_bar`
# reads rc 0 plus the value.
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --image) shift; export CW_WORK_IMAGE="${1:-}" ;;
    *) args+=("$1") ;;
  esac
  shift
done
"$(dirname "${BASH_SOURCE[0]}")/program-lift.sh" "${args[@]}" cw-work
rc=$?
[ "$rc" = 1 ] && exit 0
exit "$rc"

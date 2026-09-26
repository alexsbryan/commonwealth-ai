#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-rails-lift.sh — the cw-rails lift, kept under its old name for its callers
# (quality/instruments.toml `cw-rails-lift`, the [[forbid]] reasons that cite
# it). It is `scripts/program-lift.sh --sandbox cmnwlth`; the closure rule and
# the RUN smoke live in scripts/program-lift.toml.
#
#   scripts/cw-rails-lift.sh --sandbox [--invite <link>] [--dir <path>] [--keep]
#
# The last stdout line is program-lift's verdict line (scripts/lib/judgement.py),
# which also carries {"value": 1|0}. A measured failure exits 0 here, as it
# always did, because co-lineage.py `measure_bar` reads rc 0 plus the value.
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --invite) shift; export CW_RAILS_INVITE="${1:-}" ;;
    *) args+=("$1") ;;
  esac
  shift
done
"$(dirname "${BASH_SOURCE[0]}")/program-lift.sh" "${args[@]}" cmnwlth
rc=$?
[ "$rc" = 1 ] && exit 0
exit "$rc"

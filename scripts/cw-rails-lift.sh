#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-rails-lift.sh — the cw-rails lift, kept under its old name for its callers
# (quality/instruments.toml `cw-rails-lift`, the [[forbid]] reasons that cite
# it). It is `scripts/program-lift.sh --sandbox cmnwlth`; the closure rule and
# the RUN smoke live in scripts/program-lift.toml.
#
#   scripts/cw-rails-lift.sh --sandbox [--dir <path>] [--keep]
#
# The RUN founds its own two-node mesh in the sandbox (phase-b pb-membership),
# so it takes no invite; `--invite` is refused as unknown by program-lift.
#
# The last stdout line is program-lift's verdict line (scripts/lib/judgement.py),
# which also carries {"value": 1|0}. A measured failure exits 0 here, as it
# always did, because co-lineage.py `measure_bar` reads rc 0 plus the value.
"$(dirname "${BASH_SOURCE[0]}")/program-lift.sh" "$@" cmnwlth
rc=$?
[ "$rc" = 1 ] && exit 0
exit "$rc"

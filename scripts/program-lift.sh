#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# program-lift.sh — does a program build, test and RUN outside this monorepo?
#
#   scripts/program-lift.sh --sandbox <lift> [--dir <path>] [--keep]
#                           [--target-dir <path>] [--set VAR=value ...]
#   scripts/program-lift.sh --run-only <lift> [--dir <path>] [--keep] [--set ...]
#     (builds in this workspace and runs the RUN smoke alone; its verdict line
#      is `program-lift:<lift>:run-only`, never the lift's)
#
# The one "runs alone" decider for the six programs (FIVE_PROGRAMS §12 "Done",
# phase-b-2). The lifts and their RUN smokes are data in
# scripts/program-lift.toml; the instrument is scripts/lib/program_lift.py,
# which says its verdict on the last stdout line through
# scripts/lib/judgement.py and exits 0 passed, 1 failed, 3 could-not-judge,
# 4 never-ran, 2 usage.
exec python3 "$(dirname "${BASH_SOURCE[0]}")/lib/program_lift.py" "$@"

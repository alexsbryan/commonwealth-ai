#!/usr/bin/env sh
# Sovereign install script — placeholder until the first build ships.
#
# When the desktop binaries are ready, this script will detect the host
# platform, fetch the matching release artifact, verify its signature, and
# drop the binary into ~/.local/bin (or /usr/local/bin if writable).
#
# For now: tell the runner the project isn't ready and link to the signup.

set -eu

printf '\n'
printf '  ┌─────────────────────────────────────────────────────────┐\n'
printf '  │  sovereign — the model runs on your machine             │\n'
printf '  │                                                         │\n'
printf '  │  the desktop build is not ready yet.                    │\n'
printf '  │                                                         │\n'
printf '  │  drop your email at  https://svrnme.sh                  │\n'
printf '  │  and we will let you know exactly once.                 │\n'
printf '  │                                                         │\n'
printf '  │  source:  https://github.com/alexsbryan/commonwealth-ai │\n'
printf '  └─────────────────────────────────────────────────────────┘\n'
printf '\n'

exit 0

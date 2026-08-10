#!/usr/bin/env bash
# Every self-test for the release drivers in scripts/.
#
# These exercise the REAL release scripts with `gh` and `podman` stubbed, in a
# mktemp repo — so they prove behaviour rather than restating it, and nothing
# is uploaded, started, or written outside the temp dir (ARCH §12.4).
#
# They are shell, not `cargo test`, for the same reason .claude/hooks/tests
# are: the thing under test IS a shell script plus a subprocess boundary, and
# no Rust test can reach the seam where a release actually goes wrong.
#
#   scripts/tests/run-all.sh                          # all suites
#   bash scripts/tests/release-provenance-gate.sh     # one suite
#
# Wired into scripts/pre-push.sh, which runs this whenever a release driver or
# one of these suites changes. An opt-in guard decays into decoration.
#
# Cost: a couple of seconds. No cargo, no network, no containers, no models.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

rc=0
for suite in scripts/tests/*.sh; do
    case "$suite" in */run-all.sh) continue ;; esac
    # </dev/null: a suite drives the real drivers, and a driver that reaches a
    # command which reads STDIN would otherwise BLOCK on the terminal — the
    # gate hangs instead of failing, and in a pre-push hook that looks like a
    # frozen push. Give every suite a closed stdin so such a bug surfaces as a
    # failure here rather than as a hang. (Found exactly this way: an empty
    # sidecar glob turned `cat` into a stdin read in release-cli-local.sh.)
    bash "$suite" </dev/null || rc=1
done

if [ "$rc" -eq 0 ]; then
    echo "ALL RELEASE SCRIPT SUITES GREEN"
else
    echo "SOME RELEASE SCRIPT SUITES FAILED"
fi
exit "$rc"

#!/usr/bin/env bash
# install-journey-nightly.sh — install the nightly CLI journey lane as a
# systemd USER timer.
#
# Why a timer and not a README line: the harness this replaced was never run
# once, because running it was opt-in and nothing ever opted in. A gate that
# depends on somebody remembering is a gate that reports green forever. This
# is the half of that fix that needs no memory; scripts/pre-push.sh gate 4 is
# the other half.
#
# User timer, not system: everything it touches ($HOME/.svrnmesh, the model
# files, the dev toolbox) is per-user, and a system unit would need root for
# no benefit.
#
#   scripts/install-journey-nightly.sh            # install + enable
#   scripts/install-journey-nightly.sh --status   # is it armed? when next?
#   scripts/install-journey-nightly.sh --uninstall
#
# It only ever touches units named sovereign-journey-nightly.*. It does not
# go near sovereign.service or anything else already installed.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO_ROOT/scripts/systemd"
DEST="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNITS=(sovereign-journey-nightly.service sovereign-journey-nightly.timer)

have_systemd() { command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; }

case "${1:-install}" in
--status)
  have_systemd || { echo "no systemd here"; exit 2; }
  systemctl --user status sovereign-journey-nightly.timer --no-pager 2>&1 | head -12
  echo
  systemctl --user list-timers sovereign-journey-nightly.timer --no-pager 2>&1 | head -4
  echo
  latest="$HOME/.svrnmesh/journey-nightly/latest.json"
  if [ -f "$latest" ]; then echo "last run: $(cat "$latest")"; else echo "last run: none yet"; fi
  exit 0
  ;;
--uninstall)
  have_systemd || { echo "no systemd here"; exit 2; }
  systemctl --user disable --now sovereign-journey-nightly.timer 2>/dev/null
  rm -f "${UNITS[@]/#/$DEST/}"
  systemctl --user daemon-reload
  echo "removed the nightly journey timer (reports under ~/.svrnmesh/journey-nightly kept)"
  exit 0
  ;;
install) ;;
*) echo "usage: $0 [install|--status|--uninstall]" >&2; exit 2 ;;
esac

# Fail here rather than installing a unit that points at a script that is not
# there — a timer firing into a missing ExecStart is a nightly failure email
# about the installer, not about the code.
NIGHTLY="$REPO_ROOT/sovereign/scripts/cli-journey-nightly.sh"
[ -x "$NIGHTLY" ] || { echo "install: $NIGHTLY missing or not executable" >&2; exit 2; }

if ! have_systemd; then
  cat >&2 <<EOF
install: no systemd user session here.

Run the lane from whatever scheduler this machine does have:
    $NIGHTLY
It is safe to run unattended and writes ~/.svrnmesh/journey-nightly/latest.log.
EOF
  exit 2
fi

mkdir -p "$DEST"
for u in "${UNITS[@]}"; do
  # Substitute rather than symlink: the unit has to carry an absolute path to
  # THIS checkout, and a symlinked template would leave @REPO_ROOT@ literal.
  sed "s#@REPO_ROOT@#$REPO_ROOT#g" "$SRC/$u" > "$DEST/$u" || exit 2
  echo "installed $DEST/$u"
done

systemctl --user daemon-reload
systemctl --user enable --now sovereign-journey-nightly.timer || exit 2

# `enable` on a machine with no lingering only arms the timer while the user
# is logged in. Say so plainly instead of implying a guarantee that is not
# there — an unattended lane that only runs when you are already at the
# keyboard is most of the way back to opt-in.
if ! loginctl show-user "$(id -un)" -p Linger 2>/dev/null | grep -q 'Linger=yes'; then
  echo
  echo "NOTE: lingering is OFF for $(id -un), so user timers only run while you"
  echo "      are logged in. To let the nightly lane run regardless:"
  echo "          sudo loginctl enable-linger $(id -un)"
fi

echo
echo "armed. next run:"
systemctl --user list-timers sovereign-journey-nightly.timer --no-pager 2>&1 | sed -n '1,3p'
echo
echo "  run it now:   systemctl --user start sovereign-journey-nightly.service"
echo "  read it:      cat ~/.svrnmesh/journey-nightly/latest.log"
echo "  is it armed:  $0 --status"

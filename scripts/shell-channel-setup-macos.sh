#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# shell-channel-setup-macos.sh — NO-SUDO server-side setup for an INTERACTIVE
# ssh/tmux listener on a mesh Mac. Sibling of ops-channel-setup-macos.sh:
# same LaunchAgent + user-level sshd + tailnet-only-bind pattern, different
# grant. The ops channel on :2222 is deliberately no-shell (restrict + forced
# command); this one on :2223 is the opposite — a real pty, a real shell, for
# driving tmux sessions from a phone or a peer.
#
#   bash scripts/shell-channel-setup-macos.sh                  # default keys
#   bash scripts/shell-channel-setup-macos.sh "ssh-ed25519 AAAA... more"  # + extra
#
# Keys are APPENDED IF ABSENT, never clobbered — unlike the ops script, whose
# `>` silently drops any key you added by hand on the next run.
#
# Teardown:
#   launchctl bootout gui/$(id -u)/com.svrn.shell-sshd
#   rm -rf ~/.svrn-shell ~/Library/LaunchAgents/com.svrn.shell-sshd.plist

set -euo pipefail
[ "$(uname)" = "Darwin" ] || { echo "this script is for macOS (the server side)"; exit 1; }

PORT=2223
DIR="$HOME/.svrn-shell"
LABEL="com.svrn.shell-sshd"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

# Clients allowed an interactive shell here.
KEYS=(
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILtV9rpWPyhRaN7Uvu9+FGrP5pM2h9Ny4opfG+jRwh40 termius-iphone14'
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGyPX4NPjy7Z8f/ZHMM1bFtjz79/rkJGPw4GLR9oDr3r ruggedfox'
)
[ $# -gt 0 ] && KEYS+=("$@")

mkdir -p "$DIR" && chmod 700 "$DIR"
[ -f "$DIR/host_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$DIR/host_key"

# --- tailnet-only bind (same resolution order as the ops channel) ----------
tailnet_addr() {
  [ -n "${SHELL_LISTEN_ADDR:-}" ] && { printf '%s' "$SHELL_LISTEN_ADDR"; return 0; }
  for ts in tailscale /usr/local/bin/tailscale \
            /Applications/Tailscale.app/Contents/MacOS/Tailscale; do
    if command -v "$ts" >/dev/null 2>&1; then
      ip="$("$ts" ip -4 2>/dev/null | head -1)"
      [ -n "$ip" ] && { printf '%s' "$ip"; return 0; }
    fi
  done
  ip="$(ifconfig 2>/dev/null | awk '/inet 100\./{print $2; exit}')"
  [ -n "$ip" ] && { printf '%s' "$ip"; return 0; }
  return 1
}
TS_ADDR="$(tailnet_addr)" || {
  echo "ERROR: no tailnet address found — refusing to bind all interfaces." >&2
  echo "Bring Tailscale up, or set SHELL_LISTEN_ADDR=<ip> to bind deliberately." >&2
  exit 1
}
echo "binding shell-channel to tailnet $TS_ADDR (+ 127.0.0.1 for local checks)"

cat > "$DIR/sshd_config" <<EOF
Port $PORT
ListenAddress $TS_ADDR
ListenAddress 127.0.0.1
HostKey $DIR/host_key
PidFile $DIR/sshd.pid
AuthorizedKeysFile $DIR/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PermitTTY yes
# scp/sftp from Termius, so config and logs can move without a second channel.
Subsystem sftp /usr/libexec/sftp-server
AcceptEnv LANG LC_*
LogLevel VERBOSE
EOF

# --- authorized_keys: append-if-absent, keyed on the base64 blob -----------
touch "$DIR/authorized_keys"
for k in "${KEYS[@]}"; do
  blob="$(printf '%s' "$k" | awk '{print $2}')"
  if grep -qF "$blob" "$DIR/authorized_keys"; then
    echo "  key already authorized: $(printf '%s' "$k" | awk '{print $3}')"
  else
    printf '%s\n' "$k" >> "$DIR/authorized_keys"
    echo "  authorized: $(printf '%s' "$k" | awk '{print $3}')"
  fi
done
chmod 600 "$DIR/authorized_keys" "$DIR/host_key"

mkdir -p "$HOME/Library/LaunchAgents"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>/usr/sbin/sshd</string>
    <string>-D</string>
    <string>-f</string><string>$DIR/sshd_config</string>
    <string>-E</string><string>$DIR/sshd.log</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
EOF

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"

sleep 1
echo "--- state ---"
launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -m1 'state' || true
LISTEN="$(lsof -nP -iTCP:$PORT -sTCP:LISTEN 2>/dev/null | grep sshd || true)"
if [ -z "$LISTEN" ]; then
  echo "NOT LISTENING YET — check $DIR/sshd.log (allow sshd if the firewall prompts)"
  echo "note: if the tailnet address changed, re-run this script — sshd cannot"
  echo "      bind a ListenAddress that no longer exists on this host."
  exit 1
fi
if printf '%s\n' "$LISTEN" | awk '{print $9}' | grep -qE "^\*:$PORT\$"; then
  echo "ERROR: sshd bound *:$PORT (all interfaces) — expected $TS_ADDR only." >&2
  printf '%s\n' "$LISTEN" >&2
  exit 1
fi

echo "OK: shell-channel sshd listening on :$PORT (interactive, pty allowed)"
printf '%s\n' "$LISTEN" | awk '{print "     bound: " $9}'
echo "     reachable over the tailnet only — clients connect to $TS_ADDR:$PORT"
echo "     authorized keys:"
ssh-keygen -lf "$DIR/authorized_keys" 2>/dev/null | sed 's/^/       /'

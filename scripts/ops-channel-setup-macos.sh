#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# ops-channel-setup-macos.sh — one-shot, NO-SUDO server-side setup for the
# sandboxed ops channel (docs/OPS_CHANNEL.md). Run as the login user on the
# Mac that should ACCEPT ops connections:
#
#   bash scripts/ops-channel-setup-macos.sh            # authorize the default svrn-ops key
#   bash scripts/ops-channel-setup-macos.sh "ssh-ed25519 AAAA... comment"   # other client key
#
# What it does (all user-level, idempotent, reversible):
#   - runs a PRIVATE sshd on port 2222 as this user via a LaunchAgent
#     (system Remote Login / port 22 stays OFF; /usr/sbin/sshd ships with macOS)
#   - authorizes exactly ONE key, locked with restrict + forced command to
#     scripts/ops-channel.sh (verb allowlist, no shell, audited)
# Teardown:
#   launchctl bootout gui/$(id -u)/com.svrn.ops-sshd
#   rm -rf ~/.svrn-ops ~/Library/LaunchAgents/com.svrn.ops-sshd.plist

set -euo pipefail

[ "$(uname)" = "Darwin" ] || { echo "this script is for macOS (the server side)"; exit 1; }

DEFAULT_KEY='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII54RkuTeAumX6oc+gFevl6opDD9vrDdzyNMmEMD/+Sz svrn-ops'
CLIENT_KEY="${1:-$DEFAULT_KEY}"

HERE="$(cd "$(dirname "$0")" && pwd)"
WRAPPER="$HERE/ops-channel.sh"
[ -f "$WRAPPER" ] || { echo "missing $WRAPPER — pull the repo first"; exit 1; }
chmod +x "$WRAPPER"

OPS="$HOME/.svrn-ops"
PLIST="$HOME/Library/LaunchAgents/com.svrn.ops-sshd.plist"
LABEL="com.svrn.ops-sshd"

mkdir -p "$OPS" && chmod 700 "$OPS"
[ -f "$OPS/host_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$OPS/host_key"

cat > "$OPS/sshd_config" <<EOF
Port 2222
HostKey $OPS/host_key
PidFile $OPS/sshd.pid
AuthorizedKeysFile $OPS/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
LogLevel VERBOSE
EOF

printf 'restrict,command="%s" %s\n' "$WRAPPER" "$CLIENT_KEY" > "$OPS/authorized_keys"
chmod 600 "$OPS/authorized_keys" "$OPS/host_key"

mkdir -p "$HOME/Library/LaunchAgents"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>/usr/sbin/sshd</string>
    <string>-D</string>
    <string>-f</string><string>$OPS/sshd_config</string>
    <string>-E</string><string>$OPS/sshd.log</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
EOF

# Re-bootstrap cleanly if already loaded (config may have changed).
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"

sleep 1
echo "--- state ---"
launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -m1 'state' || true
if lsof -nP -iTCP:2222 -sTCP:LISTEN 2>/dev/null | grep -q sshd; then
  echo "OK: ops-channel sshd listening on :2222 (forced command: $WRAPPER)"
  echo "audit log will land in ~/.sovereign/logs/ops-channel.log"
else
  echo "NOT LISTENING YET — check $OPS/sshd.log (and allow sshd if the firewall prompts)"
  exit 1
fi

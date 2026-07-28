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
#   - binds ONLY the tailnet address + loopback, never 0.0.0.0 — peers reach
#     this over Tailscale, so there is no reason to answer on the LAN. Override
#     with OPS_LISTEN_ADDR=<ip>; the script FAILS rather than falling back to
#     all-interfaces if it cannot determine a tailnet address.
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

# --- tailnet-only bind ---------------------------------------------------
# Resolve this node's 100.64.0.0/10 (CGNAT) address. Tried in order: explicit
# override, the CLI on PATH, the app bundle's CLI, then an interface scan.
tailnet_addr() {
  [ -n "${OPS_LISTEN_ADDR:-}" ] && { printf '%s' "$OPS_LISTEN_ADDR"; return 0; }
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
  echo "Bring Tailscale up, or set OPS_LISTEN_ADDR=<ip> to bind deliberately." >&2
  exit 1
}
echo "binding ops-channel to tailnet $TS_ADDR (+ 127.0.0.1 for local checks)"

cat > "$OPS/sshd_config" <<EOF
Port 2222
ListenAddress $TS_ADDR
ListenAddress 127.0.0.1
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
LISTEN="$(lsof -nP -iTCP:2222 -sTCP:LISTEN 2>/dev/null | grep sshd || true)"
if [ -z "$LISTEN" ]; then
  echo "NOT LISTENING YET — check $OPS/sshd.log (and allow sshd if the firewall prompts)"
  echo "note: if the tailnet address changed, re-run this script — sshd cannot"
  echo "      bind a ListenAddress that no longer exists on this host."
  exit 1
fi

# A wildcard bind means the ListenAddress lines did not take: fail loud rather
# than quietly exposing the ops channel to every interface.
if printf '%s\n' "$LISTEN" | awk '{print $9}' | grep -qE '^\*:2222$'; then
  echo "ERROR: sshd bound *:2222 (all interfaces) — expected $TS_ADDR only." >&2
  printf '%s\n' "$LISTEN" >&2
  exit 1
fi

echo "OK: ops-channel sshd listening on :2222 (forced command: $WRAPPER)"
printf '%s\n' "$LISTEN" | awk '{print "     bound: " $9}'
echo "     reachable over the tailnet only — client connects to $TS_ADDR:2222"
echo "audit log will land in ~/.sovereign/logs/ops-channel.log"

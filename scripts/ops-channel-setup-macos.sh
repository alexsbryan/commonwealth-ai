#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# ops-channel-setup-macos.sh — one-shot, NO-SUDO server-side setup for the
# sandboxed ops channel (docs/OPS_CHANNEL.md). Run as the login user on the
# Mac that should ACCEPT ops connections:
#
#   bash scripts/ops-channel-setup-macos.sh            # keys from ~/.svrn-ops/clients
#   bash scripts/ops-channel-setup-macos.sh "ssh-ed25519 AAAA... comment"   # + one-off
#
# CLIENT KEYS ARE HOST STATE, NOT SOURCE. They live in ~/.svrn-ops/clients,
# outside the repo; the script writes a commented template there on first run
# rather than shipping a default. A key baked into a shipped script authorizes
# its owner on every machine that ever runs the script.
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

HERE="$(cd "$(dirname "$0")" && pwd)"
WRAPPER="$HERE/ops-channel.sh"
[ -f "$WRAPPER" ] || { echo "missing $WRAPPER — pull the repo first"; exit 1; }
chmod +x "$WRAPPER"

OPS="$HOME/.svrn-ops"
PLIST="$HOME/Library/LaunchAgents/com.svrn.ops-sshd.plist"
LABEL="com.svrn.ops-sshd"

mkdir -p "$OPS" && chmod 700 "$OPS"
CLIENTS="$OPS/clients"

# Seed from an existing authorized_keys so a working host is never locked out;
# otherwise write the template and stop. Stopping is the point: with no default
# client there is no accidental grant on someone else's machine.
if [ ! -f "$CLIENTS" ]; then
  if [ -s "$OPS/authorized_keys" ]; then
    awk '{for(i=1;i<=NF;i++) if($i ~ /^(ssh-(rsa|dss|ed25519)|ecdsa-|sk-)/){
           out=$i; for(j=i+1;j<=NF;j++) out=out" "$j; print out; break}}' \
      "$OPS/authorized_keys" > "$CLIENTS"
    echo "  migrated $(wc -l < "$CLIENTS" | tr -d ' ') key(s) from authorized_keys -> $CLIENTS"
  else
    cat > "$CLIENTS" <<'TEMPLATE'
# Clients authorized for the ops channel on this host, one per line in
# authorized_keys format (type, base64, comment). Blank lines and # are ignored.
#
# Do NOT add options here. The setup script adds `restrict` and the forced
# command itself; an option written here would weaken the sandbox silently.
#
# This file is deliberately outside the repo. A client key committed to a
# shipped script authorizes its owner on every machine that runs it.
#
#   ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... svrn-ops
TEMPLATE
    chmod 600 "$CLIENTS"
    echo "no clients configured — wrote a template to $CLIENTS" >&2
    echo "add the public key(s) allowed to use the ops channel, then re-run." >&2
    exit 2
  fi
fi
chmod 600 "$CLIENTS"

KEYS=()
while IFS= read -r line; do
  case "$line" in ''|'#'*) continue ;; esac
  KEYS+=("$line")
done < "$CLIENTS"
[ $# -gt 0 ] && KEYS+=("$@")
if [ "${#KEYS[@]}" -eq 0 ]; then
  echo "no clients in $CLIENTS — nothing would be authorized. Refusing." >&2
  exit 2
fi

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

# The clients file is the source of truth, so regenerating is correct here —
# unlike before, when `>` silently dropped keys added by hand to this file.
: > "$OPS/authorized_keys"
for k in "${KEYS[@]}"; do
  printf 'restrict,command="%s" %s\n' "$WRAPPER" "$k" >> "$OPS/authorized_keys"
  echo "  authorized (forced command): $(printf '%s' "$k" | awk '{print $3}')"
done
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
echo "audit log will land in ~/.svrnmesh/logs/ops-channel.log"

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
#   bash scripts/shell-channel-setup-macos.sh                  # keys from ~/.svrn-shell/clients
#   bash scripts/shell-channel-setup-macos.sh "ssh-ed25519 AAAA... more"  # + one-off
#
# CLIENT KEYS ARE HOST STATE, NOT SOURCE. They live in ~/.svrn-shell/clients,
# which is not in the repo, and the script writes a commented template there on
# first run rather than shipping a default. A key baked into a shipped script
# authorizes its owner on every machine that ever runs the script — which is a
# backdoor in shape even when it is only an artifact of personal use, and it
# also means the script works for nobody but its author.
#
# THE GRANT IS FULL SHELL AS THIS USER. Every key below can do anything you can
# do on this machine. That is categorically different from the ops channel's
# forced command, so the two must never share a key.
#
# Three things narrow it, none of which relies on anyone remembering:
#   - AllowUsers pins the SOURCE to the tailnet range, enforced at auth. The
#     ListenAddress pin is the first line; this is what still holds if the bind
#     ever goes wrong. Verified by falsification: the same key that works from
#     the tailnet is refused from 127.0.0.1.
#   - PermitOpen scopes tunnelling to the daemon. Without it a key holder can
#     reach every loopback service on this host, and loopback services are
#     written assuming loopback means trusted.
#   - expiry-time makes each key die on its own. There is no revocation path
#     from a phone, so the only closure loop available is one that runs without
#     you. Re-run this script to renew; KEY_EXPIRY_DAYS=N to change the window.
#
# authorized_keys is regenerated from that file on every run, so removing a key
# there actually revokes it. An append-if-absent scheme cannot revoke anything.
#
# The listener is run by scripts/tailnet-sshd-run.sh, not by launchd directly:
# sshd keeps running when a ListenAddress is absent, so the naive plist loses a
# boot race with Tailscale and serves loopback only, forever, while every check
# reports healthy. The wrapper waits for the address and rebinds when it moves.
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
HERE="$(cd "$(dirname "$0")" && pwd)"
RUNNER="$HERE/tailnet-sshd-run.sh"
[ -f "$RUNNER" ] || { echo "missing $RUNNER — pull the repo first"; exit 1; }
chmod +x "$RUNNER"

KEY_EXPIRY_DAYS="${KEY_EXPIRY_DAYS:-90}"
EXPIRES="$(date -v+"${KEY_EXPIRY_DAYS}"d +%Y%m%d)"
ME="$(id -un)"
mkdir -p "$DIR" && chmod 700 "$DIR"
CLIENTS="$DIR/clients"

# Seed from an existing authorized_keys so an already-working host is never
# locked out by this change; otherwise write the template and stop. Stopping is
# the point — there is no default client, so there is no accidental grant.
if [ ! -f "$CLIENTS" ]; then
  if [ -s "$DIR/authorized_keys" ]; then
    awk '{for(i=1;i<=NF;i++) if($i ~ /^(ssh-(rsa|dss|ed25519)|ecdsa-|sk-)/){
           out=$i; for(j=i+1;j<=NF;j++) out=out" "$j; print out; break}}' \
      "$DIR/authorized_keys" > "$CLIENTS"
    echo "  migrated $(wc -l < "$CLIENTS" | tr -d ' ') key(s) from authorized_keys -> $CLIENTS"
  else
    cat > "$CLIENTS" <<'TEMPLATE'
# Clients authorized for an interactive shell on this host, one per line in
# authorized_keys format (type, base64, comment). Blank lines and # are ignored.
#
# Do NOT add options here. The setup script adds expiry-time itself, and a
# stray command= or restrict would silently change the grant.
#
# This file is deliberately outside the repo. A client key committed to a
# shipped script authorizes its owner on every machine that runs it.
#
#   ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... my-phone
TEMPLATE
    chmod 600 "$CLIENTS"
    echo "no clients configured — wrote a template to $CLIENTS" >&2
    echo "add the public key(s) allowed to log in here, then re-run." >&2
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
[ -f "$DIR/host_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$DIR/host_key"

# LogLevel VERBOSE is worth keeping (it names which key authenticated), but it
# is also why the ops channel's log reached 1MB unattended. Roll at setup.
if [ -f "$DIR/sshd.log" ] && [ "$(wc -c < "$DIR/sshd.log")" -gt 2000000 ]; then
  mv -f "$DIR/sshd.log" "$DIR/sshd.log.1"
  echo "  rolled sshd.log (>2MB) to sshd.log.1"
fi

# --- tailnet address ------------------------------------------------------
# Resolved here only to REPORT where clients should connect and to fail early
# if Tailscale is down. The runtime bind is the wrapper's job: it re-resolves
# on every start and writes $DIR/listen.conf, which sshd_config Includes.
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

# Persist a deliberate override so the RUNTIME honours it. SHELL_LISTEN_ADDR is
# an env var only here, at setup; the launchd job inherits none of this shell's
# environment, so the wrapper reads $DIR/listen.override instead. Absent env,
# the stale file is removed rather than left to quietly outrank Tailscale.
if [ -n "${SHELL_LISTEN_ADDR:-}" ]; then
  printf '%s\n' "$SHELL_LISTEN_ADDR" > "$DIR/listen.override"
  echo "  pinned by SHELL_LISTEN_ADDR -> $DIR/listen.override"
else
  rm -f "$DIR/listen.override"
fi

# --- PATH for non-interactive commands -----------------------------------
# `ssh host <cmd>` runs `zsh -c`, which sources .zshenv only — NOT .zprofile,
# where brew shellenv lives. So `ssh mac tmux attach` and every Termius snippet
# died with "command not found" while an interactive login worked fine. SetEnv
# fixes the session PATH without touching the login shell's (path_helper and
# brew shellenv rebuild it anyway). Deliberately minimal: the full login PATH
# carries editor-extension and plugin-cache entries that have no business being
# baked into a service config.
BREW_PREFIX="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
SSH_PATH="$BREW_PREFIX/bin:$BREW_PREFIX/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin:$HOME/.cargo/bin"

umask 077
printf '# generated by tailnet-sshd-run.sh — do not edit\nListenAddress %s\nListenAddress 127.0.0.1\n' \
  "$TS_ADDR" > "$DIR/listen.conf"

cat > "$DIR/sshd_config" <<EOF
Port $PORT
# The ONLY generated line lives here; the wrapper rewrites it whenever the
# tailnet address moves. Never add a bare ListenAddress to this file.
Include $DIR/listen.conf
HostKey $DIR/host_key
PidFile $DIR/sshd.pid
AuthorizedKeysFile $DIR/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes yes
PermitTTY yes
# Termius roams cell<->wifi and drops TCP without a FIN. Without these the
# server accumulates sessions that will never speak again, and the client hangs
# on a socket that is already dead.
ClientAliveInterval 30
ClientAliveCountMax 4
TCPKeepAlive yes
LoginGraceTime 30
MaxAuthTries 4
# Enforce the tailnet-only claim at AUTH, not just at bind. If the wrapper ever
# misfires, or a future edit reintroduces a bare ListenAddress, this still holds.
AllowUsers $ME@100.64.0.0/10 $ME@127.0.0.1
# Termius port-forwards are worth having (reaching the daemon from a phone) but
# unrestricted they reach EVERY loopback service — sccache, the VS Code tunnel
# helpers, and the ops channel's own sshd. Name the one that is wanted.
AllowTcpForwarding yes
PermitOpen 127.0.0.1:9741 localhost:9741
AllowAgentForwarding no
X11Forwarding no
# scp/sftp from Termius, so config and logs can move without a second channel.
Subsystem sftp /usr/libexec/sftp-server
AcceptEnv LANG LC_*
SetEnv PATH=$SSH_PATH
LogLevel VERBOSE
EOF
chmod 600 "$DIR/sshd_config" "$DIR/listen.conf"

# --- authorized_keys: append-if-absent, keyed on the base64 blob -----------
# authorized_keys is REGENERATED from the clients file, which is the source of
# truth. Anything else is append-only: a key dropped from clients would keep
# working forever, which is the failure this file was reorganised to prevent.
# Do not hand-edit authorized_keys — edit clients and re-run.
: > "$DIR/authorized_keys"
for k in "${KEYS[@]}"; do
  printf 'expiry-time="%s" %s\n' "$EXPIRES" "$k" >> "$DIR/authorized_keys"
  echo "  authorized: $(printf '%s' "$k" | awk '{print $3}') (expires $EXPIRES)"
done
chmod 600 "$DIR/authorized_keys" "$DIR/host_key"

# A bad config here surfaces later as a service that simply never listens, which
# reads like a firewall or a bind problem. Fail at setup instead.
/usr/sbin/sshd -t -f "$DIR/sshd_config" || {
  echo "ERROR: generated sshd_config failed 'sshd -t' — not bootstrapping." >&2
  exit 1
}

mkdir -p "$HOME/Library/LaunchAgents"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>$RUNNER</string>
    <string>$DIR</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$DIR/agent.log</string>
  <key>StandardErrorPath</key><string>$DIR/agent.log</string>
</dict></plist>
EOF

# bootout is asynchronous: bootstrapping before the old job is reaped fails with
# a bare "Bootstrap failed: 5: Input/output error". Wait for it to go.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
for _ in $(seq 1 30); do
  launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || break
  sleep 0.2
done
launchctl bootstrap "gui/$(id -u)" "$PLIST"

# --- verify: assert the tailnet bind POSITIVELY ---------------------------
# The old check only ruled out a wildcard bind, which passes in the exact state
# this whole rewrite exists to prevent — sshd up, loopback bound, tailnet bind
# refused, "OK ... reachable over the tailnet" printed about nothing.
echo "--- state ---"
LISTEN=""
for _ in $(seq 1 40); do
  LISTEN="$(lsof -nP -iTCP:$PORT -sTCP:LISTEN 2>/dev/null | grep sshd || true)"
  printf '%s\n' "$LISTEN" | awk '{print $9}' | grep -qxF "$TS_ADDR:$PORT" && break
  sleep 0.5
done
launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -m1 'state' || true

if [ -z "$LISTEN" ]; then
  echo "NOT LISTENING — check $DIR/sshd.log and $DIR/agent.log" >&2
  echo "(allow sshd if the macOS firewall prompts)" >&2
  exit 1
fi
if printf '%s\n' "$LISTEN" | awk '{print $9}' | grep -qE "^\*:$PORT\$"; then
  echo "ERROR: sshd bound *:$PORT (all interfaces) — expected $TS_ADDR only." >&2
  printf '%s\n' "$LISTEN" >&2
  exit 1
fi
if ! printf '%s\n' "$LISTEN" | awk '{print $9}' | grep -qxF "$TS_ADDR:$PORT"; then
  echo "ERROR: sshd is up but NOT bound to $TS_ADDR:$PORT — no peer can reach it." >&2
  printf '%s\n' "$LISTEN" | awk '{print "     bound instead: " $9}' >&2
  grep -i 'bind' "$DIR/sshd.log" 2>/dev/null | tail -3 >&2
  exit 1
fi

echo "OK: shell-channel sshd listening on :$PORT (interactive, pty allowed)"
printf '%s\n' "$LISTEN" | awk '{print "     bound: " $9}'
echo "     reachable over the tailnet only — clients connect to $TS_ADDR:$PORT"
echo "     authorized keys:"
ssh-keygen -lf "$DIR/authorized_keys" 2>/dev/null | sed 's/^/       /'

# Ops channel — sandboxed SSH between mesh machines

**Opened 2026-07-27** for the DISTRIBUTED_PILOT_READINESS P0-knockout runs. Gives an
agent (or human) on one mesh machine a **narrow, audited, no-shell** command surface
on a peer — enough to observe daemons, tail logs, and drive the destructive
acceptance tests (`daemon-kill9`, restarts) remotely, without handing out shell access.

**Why SSH and not the mesh:** any mesh-riding channel (iroh bridge, gossip, notes
sync) dies with the daemon — and the P0.4/P0.5 acceptance tests kill daemons on
purpose. SSH is the out-of-band path that stays up exactly when the mesh is down.

## Security model

- **Dedicated keypair** (`~/.ssh/svrn_ops_ed25519` on the client), used for nothing else.
- The server's `authorized_keys` entry carries **`restrict`** (no pty, no port/agent/X11
  forwarding) **and a forced command** — the client never gets a shell. Whatever it
  sends lands in `SSH_ORIGINAL_COMMAND` and is matched against a fixed verb allowlist
  in `scripts/ops-channel.sh`. Nothing from the client is ever eval'd.
- Arguments are regex-validated (e.g. `logs N` requires pure digits — `logs 50; rm -rf /`
  is rejected; proven in the 2026-07-27 shakedown).
- **Every invocation, allowed or rejected, is appended to `~/.sovereign/logs/ops-channel.log`**
  with timestamp, client IP, and the raw command string.
- **Revocation = delete one line** from `authorized_keys`.

## Verbs

| Verb | What it does |
|---|---|
| `ping` | Channel test: `ok host=<name> time=<utc>` |
| `status` / `mesh-status` / `transport` | The corresponding `sovereign` CLI reads |
| `http-status` / `mesh-http` | `curl 127.0.0.1:9741/status` / `/v1/mesh/status` |
| `logs [N]` | Last N lines (default 200, cap 5000) of the newest daemon log |
| `cache-size` | `du -sh` of `~/.sovereign/*cache*` + disk headroom |
| `exe-info` | Daemon pid, **executable path + mtime, uptime** — catches the stale-binary trap (a rebuilt binary shows the running one as `(deleted)` on Linux) |
| `git-head` | Repo HEAD + short dirty status |
| `daemon-start` / `daemon-stop` / `daemon-restart` | Via the `sovereign` CLI |
| `daemon-kill9 [dry]` | `kill -9` the daemon (P0.4 acceptance); `dry` prints the pid it *would* kill |

## Server setup (BeefyMac) — paste-able, NO sudo

macOS ships `/usr/sbin/sshd` even with Remote Login off — Remote Login only
controls the root launchd socket on port 22. We instead run a **user-level
sshd on port 2222** via a LaunchAgent: no sudo, runs as `alexsbryan`, accepts
exactly one key, forced-command only, and system Remote Login stays OFF.
(Smaller surface than Remote Login, not a workaround.)

One command, run as the login user on the Mac (repo pulled first):

```bash
bash ~/dev/commonwealth-ai/scripts/ops-channel-setup-macos.sh
```

The script is idempotent (safe to re-run; re-bootstraps the LaunchAgent) and
prints `OK: ops-channel sshd listening on :2222` on success. It authorizes the
default svrn-ops client key (RuggedFox); pass a different pubkey as `$1` to
authorize another client. It derives the forced-command path from its own repo
location — no hardcoded home directory.

What it sets up: `~/.svrn-ops/` (host key, sshd_config, single-key
`authorized_keys`) + `~/Library/LaunchAgents/com.svrn.ops-sshd.plist` running
`/usr/sbin/sshd -D` on port 2222 as the login user.

If the macOS application firewall is on, the first incoming connection pops an
"Allow sshd?" dialog — click Allow (no admin credentials needed).

Notes:
- The sshd runs as `alexsbryan`, so logins are only possible AS `alexsbryan`,
  password auth is disabled at the config level, and the single key maps to
  the forced command. `PasswordAuthentication no` + non-root sshd means there
  is no path to a password prompt at all.
- LaunchAgents run per-login-session: after a reboot the channel is up once
  `alexsbryan` logs in (BeefyMac normally stays logged in).
- Teardown: `launchctl bootout gui/$(id -u)/com.svrn.ops-sshd` and delete
  `~/.svrn-ops` + the plist.
- Alternative (needs admin): classic Remote Login on port 22
  (`sudo systemsetup -setremotelogin on`) with the same `authorized_keys`
  line in `~/.ssh/authorized_keys` — same sandbox, bigger surface.

## Client setup (RuggedFox) — DONE 2026-07-27

- Key: `~/.ssh/svrn_ops_ed25519` (pubkey above).
- `~/.ssh/config` has `Host beefymac-ops` → `192.168.1.2` **port 2222**,
  `IdentitiesOnly`, `BatchMode`, 5s timeout. **IP, not `.local`** — mDNS is
  deliberately blocked during the P0.5 heal test, and the channel must
  survive that.

## Verification (run from RuggedFox after server setup)

```bash
ssh beefymac-ops ping              # → ok host=BeefyMac ...
ssh beefymac-ops uname -a          # → MUST be rejected ("verb not allowed"), exit 1
ssh beefymac-ops                   # bare login → runs 'ping' (forced cmd), NO shell
ssh beefymac-ops daemon-kill9 dry  # → "would kill -9 pid=<N>" (nothing killed)
ssh beefymac-ops logs 50           # → last 50 daemon log lines
```

Then on BeefyMac, confirm the audit trail: `tail ~/.sovereign/logs/ops-channel.log`
shows one line per call including the rejected `uname -a`.

## Shakedown status

- **2026-07-27 — wrapper logic proven on RuggedFox** by forced-command simulation
  (`SSH_ORIGINAL_COMMAND` env): all verbs OK, arbitrary command rejected, arg-injection
  (`logs 50; rm -rf /`) rejected by numeric validation, audit log correct, `kill9 dry`
  correct. Bonus: `exe-info` immediately caught a live stale-binary condition
  (running daemon's exe `(deleted)` after a rebuild).
- **Pending:** live e2e against BeefyMac — run the no-sudo server block above
  (user-level sshd on 2222; Remote Login stays off — port 22 was refused as of
  2026-07-27 and can stay that way), then the Verification block.

## Notes

- The wrapper is generic (macOS bash 3.2 + Linux) — the same setup works in the
  other direction (BeefyMac → RuggedFox) by generating a second dedicated key,
  though RuggedFox currently has no sshd installed (atomic Fedora; would need
  `rpm-ostree install openssh-server` or an sshd in a toolbox).
- This is deliberately NOT a general remote-admin tool. If a run needs a new
  operation, add a verb to the allowlist — do not widen an existing one.
- The mesh-native message channel (daemon inbox + `mesh_send`/`mesh_inbox` MCP
  tools over `bridge_for`) remains the right long-term surface for the ~20-person
  mesh; this SSH channel is the two-developer-machines stopgap and the
  out-of-band path for daemon-killing tests. See the remote-support arc notes.

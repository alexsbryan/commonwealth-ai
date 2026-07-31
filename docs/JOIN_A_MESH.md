# Join a mesh

A mesh is two or more machines answering as one: pooled models, pooled
knowledge, no central server. This page is the one place that takes you
from separate daemons to `[2/2 online]` — every guide that starts "on a
mesh…" assumes what's here.

You have this prerequisite when `svrn mesh status` lists every machine you
meant to pool. Already true? Go back to whatever sent you here.

## One thing to know up front

You already have a mesh. The first daemon boot after
[setup](./START_THE_DAEMON.md) quietly founds a **solo mesh** on that
machine — so "joining" is bringing machines into a mesh that exists, not
creating something new. It's also why `svrn mesh create` on an
already-set-up machine errors with "a mesh already exists": there's
nothing to create. Read the existing key instead (below); mint a fresh one
with `svrn mesh rotate`.

## What the network must allow

Between the machines: **TCP 9742** (the mesh-internal API — gossip and
knowledge fan-out) and, on a shared LAN, **UDP 5353** (mDNS discovery).
The client API (`:9741`) stays loopback unless you deliberately expose it
— and never expose `:9742` to the open internet; it has no per-request
auth by design ([threat model](./THREAT_MODEL.md) has exactly what listens
where and why). Same LAN or one tailnet (Tailscale/Headscale) both work.

## 1 — Every machine runs a daemon

On each machine, [install and set up](./START_THE_DAEMON.md), then:

```sh
svrn daemon start
svrn mesh status     # each machine shows its own solo mesh [1/1 online]
```

## 2 — Read the invite on the host

On the machine whose mesh the others will join:

```sh
svrn mesh status     # prints the join key: cwth-XXXX-XXXX-XXXX
```

## 3 — Join from each other machine

Same LAN (mDNS finds the host automatically):

```sh
svrn mesh join cwth-XXXX-XXXX-XXXX
```

Across networks, or on WiFi with client isolation, mDNS can't see the
host — hand the join an explicit relay to the host's internal port:

```sh
svrn mesh join "sovereign://join/cwth-XXXX-XXXX-XXXX?relay=<host-ip>:9742"
```

Cross-network setups (relays, tailnets, firewalls between sites) are
walked through in
[running a mesh across networks](../commonwealth/docs/getting-started.md).

## Verify you have it

```sh
svrn mesh status      # wait for [N/N online]
svrn mesh transport   # each peer's live path: direct / relayed / mixed
```

## Rotating the key

`svrn mesh rotate` mints a new join key and invalidates the old one —
members already in stay connected; only future joins need the new key.
One known rough edge (2026-07): the CLI and the daemon can disagree about
where mesh state lives, in which case a CLI-rotated key never reaches the
daemon and joiners are refused. If a freshly rotated key isn't accepted,
rotate in the daemon directly:

```sh
curl -s -X POST http://localhost:9741/v1/mesh/rotate
```

## Leaving

`svrn mesh leave` returns a machine to a solo mesh of its own. The mesh
carries on without it; nothing it hosted is left behind on the others.

## Appendix: two daemons on one machine

Possible — useful for kicking the tires without a second computer — with
two caveats the two-machine path doesn't have. Each daemon needs its own
config (distinct ports and data dir):

```toml
# node-b.toml
[daemon]
client_port = 9743
internal_port = 9744
client_bind = "127.0.0.1"
[data]
dir = "/tmp/svrn-node-b"
```

and because the `svrn mesh join` CLI talks to the daemon on `:9741`, the
second daemon joins via its own client port directly:

```sh
svrn daemon run --config node-b.toml &
curl -s -X POST http://127.0.0.1:9743/v1/mesh/join \
  -H 'content-type: application/json' \
  -d '{"key_or_url": "sovereign://join/cwth-XXXX-XXXX-XXXX?relay=127.0.0.1:9742", "node_name": "node-b"}'
```

Second caveat: there is no mDNS-off switch yet, so two bare daemons on one
machine will also discover any real mesh on your LAN. The multi-process
soak harness (`scripts/mesh-soak.sh`) solves this with a rootless network
namespace on Linux; treat the one-machine form as a dev convenience, not
the demo.

## When it breaks

- **"A mesh already exists" on `mesh create`** — expected after setup; see
  [the top of this page](#one-thing-to-know-up-front).
- **Join hangs on shared WiFi** — client isolation is blocking mDNS; use
  the explicit `?relay=<ip>:9742` join form above.
- **macOS firewall prompt** for the daemon listening on `0.0.0.0:9742` —
  allow it; that's the mesh-internal port.
- Anything else: `svrn doctor` on each machine, then the
  [troubleshooting guide](../sovereign/docs/TROUBLESHOOTING.md).

# Enterprise fleet deployment — your VPC, your network, no Tailscale

This is for running a Sovereign GPU fleet on your own private network: a cloud
VPC, an on-prem subnet, anything where the machines already reach each other by
private address and you control the firewall. No Tailscale, no mDNS multicast,
no public relay.

## The shape: star outside, mesh inside

- The **hub** your users talk to is `sovereign-server` — a plain HTTP/WS service
  that sits behind your own ingress (your load balancer terminates TLS; the
  server speaks plain HTTP to it) and authenticates users with bearer tokens.
  That's a standard web-service deployment and is documented separately
  (`ARCHITECTURE.md` §10). Nothing about it needs Tailscale.
- The **GPU fleet** behind the hub is a Commonwealth mesh of `sovereign daemon`
  nodes that coordinate to serve a shared or distributed model. This document is
  about forming and securing *that fleet* on your network.

## Forming the fleet without mDNS

By default the daemon advertises and browses `_commonwealth._tcp` over mDNS
multicast for zero-config LAN discovery. Cloud VPCs silently drop multicast, and
some hardened network namespaces won't even let the multicast socket bind (which
would otherwise fail the daemon at boot). So turn mDNS off and join by static
address instead.

On **every** node:

```toml
# ~/.sovereign/config.toml
[discovery]
mdns = false            # equivalently: set SOVEREIGN_DISABLE_MDNS=1 in the env
```

Pick one node as the **founder**. It needs no join credential — on first boot
(no persisted mesh) it creates its own mesh and logs `solo mesh created`, then
prints a join key (also written to `~/.sovereign/join_key.secret`). Copy that
key.

```toml
# founder — ~/.sovereign/config.toml
[discovery]
mdns = false
# no join_key, no seed_addrs → this node founds the mesh
```

Every other node is a **joiner**. Give it the founder's key and address:

```toml
# joiner — ~/.sovereign/config.toml
[discovery]
mdns       = false
join_key   = "cwth-a1b2-c3d4-e5f6"      # the founder's key
seed_addrs = ["10.0.1.4:9742"]           # founder's internal-API address(es)
```

On first boot a joiner POSTs `/internal/join` to each seed in turn until one
accepts, then gossip takes over. List more than one address in `seed_addrs` for
join-time failover. A joiner that has a `join_key` but can't reach any seed
**exits with an error** rather than silently founding its own mesh — that
prevents a split-brain fleet.

Once any node is in the mesh, gossip propagates the full membership, so you only
need to seed each joiner with *one* reachable existing member — not the whole
roster.

## Addresses behind NAT / ingress

A node advertises the address peers should dial it back on. By default it picks
a non-loopback interface IP. If a node sits behind NAT, a container bridge, or
port remapping, that auto-detected address won't be reachable from peers — set
it explicitly:

```
SOVEREIGN_ADVERTISE_ADDR=10.0.1.7:9742
```

On a flat VPC where private addresses are directly routable between nodes, you
don't need this.

Keep the **client port uniform** across the fleet (the default `9741`).
Inference and status routing assume every peer's client API is on the same port.

## Confidentiality is your network's job

Be deliberate about this. The mesh's internal API (`:9742`) is **plaintext and
unauthenticated** — gossip, knowledge fan-out, and model/shard transfer trust
the network boundary, not a per-request credential. When a model is split across
nodes, the raw-TCP tensor traffic between GPU boxes is plaintext as well. There
is **no wire encryption on the fleet by default**; it is designed to run inside a
private, trusted network.

So make the network that boundary:

- **Restrict `:9742` and the RPC ports to the fleet's private subnet** with your
  security groups / firewall. Nothing outside the fleet should be able to reach
  them.
- Optionally pin the internal API to a specific private interface, so it isn't
  even listening on a public NIC of a multi-homed host:

  ```toml
  [daemon]
  internal_bind = "10.0.1.7"   # default is 0.0.0.0 (every interface)
  ```

- If your platform already encrypts the network layer (an encrypted VPC,
  WireGuard, a service mesh doing mTLS between pods), the fleet rides on top of
  it transparently — that is the recommended way to get confidentiality between
  GPU boxes here.

The hub↔user edge is separate and is ordinary HTTPS: your ingress terminates
TLS, and users authenticate to `sovereign-server` with bearer tokens.

### What about the built-in `require_encryption` mode?

There is an opt-in mesh mode (`require_encryption`, founder-set at mesh creation)
that forces all mesh traffic onto the iroh QUIC transport — encrypted and dialed
by Ed25519 key — and binds the plaintext listeners loopback-only. It works, but
today it relies on public relays for NAT traversal and there is no self-hosted
relay yet, so for a single-VPC fleet where the machines reach each other
directly, your own network isolation is the simpler and stronger answer. (A
self-hostable relay is the open item for air-gapped / multi-site fleets.)

## Checklist

- [ ] `mdns = false` on every node (or `SOVEREIGN_DISABLE_MDNS=1`)
- [ ] founder boots first; you've copied its join key
- [ ] joiners have `join_key` + at least one reachable `seed_addrs`
- [ ] uniform `client_port` across the fleet
- [ ] `SOVEREIGN_ADVERTISE_ADDR` set on any NAT'd / bridged node
- [ ] security groups restrict `:9742` and the RPC ports to the fleet subnet
- [ ] (optional) `internal_bind` pinned to the private NIC
- [ ] users reach `sovereign-server` over your TLS-terminating ingress

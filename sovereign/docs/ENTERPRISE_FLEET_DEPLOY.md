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

## Forming the fleet: create, then invite

(The interactive form of everything in this section — invites, joins,
relays — is [join a mesh](../../docs/JOIN_A_MESH.md); this page is the
fleet path.)

Each node's mesh endpoint is `cw-rails`, which holds the node's key and
carries every mesh request over iroh QUIC. `svrn mesh up` brings it up and
enables the user unit that starts it at boot; `svrn mesh create` and
`svrn mesh join` run the same bring-up first.

By default cw-rails advertises and browses mDNS for zero-config LAN discovery.
Cloud VPCs silently drop multicast, so turn it off on **every** node before the
first `svrn mesh up`:

```toml
# ~/.svrnmesh/config.toml
[discovery]
mdns = false            # equivalently: set SOVEREIGN_DISABLE_MDNS=1 in the env
```

Pick one node as the **founder** and create the mesh there. It prints an
invite link:

```
svrn mesh create "GPU fleet"
```

Every other node is a **joiner**, and joins with that link:

```
svrn mesh join '<the invite link>'
```

The link carries the founder's key and its direct addresses, so a joiner on the
same VPC dials it with no relay. An invite expires after 24 hours; `svrn mesh
status` on any member prints the current one, and `svrn mesh rotate` mints a
new one. A config that still names `[discovery] join_key` is refused at boot,
naming `svrn mesh join`: the config-driven join and its `seed_addrs` went with
the plaintext mesh.

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

## Confidentiality on the fleet

Every mesh is encrypted: cw-rails founds no other kind, and it carries every
mesh request — gossip, joins, ring sync, knowledge fan-out, model files and the
tensor-split RPC — over iroh QUIC, dialled by each node's Ed25519 key and
admitted against the roster. Programs on a node register their loopback
origins with cw-rails, which forwards a member's request to them; the daemon's
internal API binds loopback only, so `[daemon] internal_bind` is no longer
bound, and nothing needs `:9742` open between nodes. The rpc worker's own
port (`:50052` by default) is a local service: keep it off the public network,
as serve reaches it for members through the mesh (`cwth/rpc/0`).

The hub↔user edge is separate and is ordinary HTTPS: your ingress terminates
TLS, and users authenticate to `sovereign-server` with bearer tokens.

### Relays and a no-third-party posture

On a flat VPC the invite's direct addresses suffice. For **air-gapped or
multi-site fleets**, point every node's cw-rails at a relay you host (the
`iroh-relay` binary on one small TLS-terminated box), and sever iroh's public
address lookup, in cw-rails' own config:

```toml
# on every node — ~/.commonwealth-rails/rails.toml (under CW_RAILS_DIR when set)
[relay]
urls = ["https://relay.internal.example:443"]
discovery = "none"                 # sever ALL contact with iroh's public services
```

`svrn mesh up` moves a node's old `[iroh] relay_urls` and `discovery` keys
there once, keeping a backup of `config.toml`. **`urls` alone is not enough for
a no-third-party posture**: by default a node also uses iroh's public DNS/pkarr
service to publish and resolve peer addresses, and `discovery = "none"` severs
that too.

The relay path is TCP/443, so it also carries the mesh where UDP egress is
blocked; if the fleet reaches out through an HTTP proxy, set
`HTTP_PROXY`/`HTTPS_PROXY` in cw-rails' environment (Basic-auth proxies are
honored via `https://user:pass@proxy:443`; NTLM/Kerberos proxies are not
supported).

## Per-user isolation on the hub

The fleet is one trust domain; your **users** are not. When many users share one
hub, the guarantee is that no user can reach another's data — across search,
corpus listings, chunk reads, documents, and RAG retrieval. Each API key maps to
a tenant (the principal); the server derives the principal per request and the
`Runtime` only ever sees an opaque corpus allow-list, never the tenant identity
(`ARCHITECTURE.md` §10).

Two corpus layers, and retrieval spans both:

- **`Org`** — shared corpora the operator installs; every user queries them.
  This is the default visibility, so single-user and operator-curated
  deployments are unaffected.
- **`Private { owner }`** — owned by one user; enters *only* that user's
  retrieval, never anyone else's.

Enforcement is a per-request **ceiling** — `{Org} ∪ {Private you own}`, computed
server-side and applied as a hard filter at every corpus search. It is
independent of the user's per-conversation corpus selection, so a client that
sends no selection, or forges one naming another tenant's private corpus, still
cannot widen retrieval past what it owns. An absent selection defaults to the
ceiling, never to "everything."

### Per-user uploads

A user turns a file into a private corpus with:

```
POST /v1/corpora/upload      Authorization: Bearer <user's API key>
{ "file_path": "/path/readable/on/the/server.md", "name": "My Notes" }
```

`file_path` is a path on the **server's** filesystem — stage the bytes there with
your upload front-end first (same shape as `/v1/documents/upload`). The server
ingests the file into a real searchable index and stamps it
`Private { owner = <that user> }`, so it is retrievable in that user's chats
alongside the `Org` corpora and invisible to everyone else. The id is
owner-namespaced (`user:<tenant>:<slug>`); re-uploading the same name updates it.

Two facts worth knowing when you operate this:

- A private corpus is **two artifacts under one id** — a LanceDB index *and* a
  `Private{owner}` record, both written by the server. If you ever touch corpus
  state by hand: an index with no record is unsearchable even by its owner, and a
  record with no index is empty.
- v1 ingests **text and Markdown**. Other formats (PDF, …) need the extraction
  the desktop app does and are a follow-on.

Identity today is a static API-key→tenant file — fine for a fixed user set. An
OIDC/JWT mode (map a token's `sub` to the principal) and per-user usage metering
are the natural next steps for a larger base.

## Checklist

- [ ] `mdns = false` on every node (or `SOVEREIGN_DISABLE_MDNS=1`)
- [ ] the founder ran `svrn mesh create`; you've copied its invite link
- [ ] each joiner ran `svrn mesh join '<link>'` inside the invite's 24 hours
- [ ] uniform `client_port` across the fleet
- [ ] `SOVEREIGN_ADVERTISE_ADDR` set on any NAT'd / bridged node
- [ ] (multi-site) every rails.toml names your relay and `discovery = "none"`
- [ ] users reach `sovereign-server` over your TLS-terminating ingress
- [ ] each user's API key maps to a distinct tenant (per-user isolation keys off it)

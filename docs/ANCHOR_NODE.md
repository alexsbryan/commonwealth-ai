# Run an anchor node

One machine holds the models everyone else codes and chats against. The
laptops keep their own daemon, their own editor, their own keys — they
just stop pretending they can decode a 27B. This page is the operator's
path from "I have a good box" to "six people are using it."

You have this when a teammate's editor, pointed at *their own*
`localhost:9741`, answers from your hardware. Already true? Go back to
whatever sent you here.

## Decide this first: member or guest

Two ways to let someone use your models, and they are not
interchangeable. Pick before you send anyone instructions.

| | **Member** | **Guest** |
|---|---|---|
| Runs a daemon | yes | no |
| Holds the mesh secret | yes, until they leave | never |
| Gets pooled knowledge, ledger, gossip | yes | no |
| Revocable | `svrn mesh forget-member` | `--revoke`, and a TTL ≤ 24h |
| **Works in an editor** | **yes** | **only on a plaintext mesh — see below** |

**For a coding group, onboard members.** The guest path looks lighter and
is the wrong tool here: a guest link is consumed by `svrn chat` and
nothing else. On an encrypted mesh it cannot drive an editor at all, for
a reason worth understanding before you promise otherwise.

### Why a guest link usually can't drive an editor

On an **encrypted** mesh the plaintext client API is closed to the
network by policy, so `svrn mesh grant` puts an iroh dial string in the
link instead of an address. Accepting it opens a QUIC tunnel that binds
a local port — but that tunnel is parked in a process-lifetime slot, and
dropping it shuts the port (`open_route`, in
`sovereign/crates/sovereign-cli-llm/src/guest_link.rs`). It lives as
long as the `svrn chat` process and no longer. There is no persistent guest
proxy command; `GuestTunnel` has exactly two callers, `open_route` and
its tests.

On a **plaintext** mesh the link carries a plain `http://host:9741` and
a bearer token, which any OpenAI client can use directly. That is the
only configuration where "paste a base_url into your editor" is true for
a guest.

Check which you have — the mint tells you in one line:

```sh
svrn mesh grant --model primary --ttl 30m --label scratch
# "Reach at: over the mesh tunnel (this mesh encrypts …)"  → encrypted
# "Reach at: http://<addr>:9741"                            → plaintext
svrn mesh grant --revoke <token>
```

## 1 — Prepare the anchor

The anchor is an ordinary node that happens to hold the big model. If
it is already [set up and running](./START_THE_DAEMON.md), you are most
of the way there.

```sh
svrn doctor                            # must be clean before anyone joins
curl -s localhost:9741/v1/models       # the aliases others will name
```

You want `primary` and `fast` in that list. Those two aliases are what
teammates put in their editor config, and they are what makes this work:
requests naming them are passed to the mesh layer **unresolved**, so the
load balancer picks whichever node advertising the alias is least busy
rather than pinning to one machine's GGUF filename
(`commonwealth/crates/commonwealth-api/src/routes_inference.rs`). A
teammate who names a concrete quant instead gets pinned to whoever has
that exact file, and loses the anchor the day you swap quants.

### Two settings before anyone connects

**Queue wait.** The slot sheds a caller whose predicted wait exceeds
`SOVEREIGN_MAX_QUEUE_WAIT_SECS` (default 30) rather than parking them.
That default is right for a mesh where another node can answer and wrong
for an anchor, where a shed means *no* answer instead of a slow one:

```sh
SOVEREIGN_MAX_QUEUE_WAIT_SECS=0   # park instead of shedding
```

Set it in the daemon's environment, not your shell — the value is read
by the daemon process. `=0` restores unbounded waiting.

**Concurrency, which is not a setting.** Each model slot serves one turn
at a time; there is no continuous batching and no `-np` equivalent. Six
developers against one anchor serialize. Say this out loud during
onboarding — it is the single most likely source of "why is it slow",
and it is architectural, not tuning. Size expectations by turn length,
not by user count.

## 2 — Onboard a member

**On their machine:**

```sh
curl -fsSL https://svrnme.sh/install.sh | sh   # drops the CLI in ~/.local/bin
svrn setup                                      # hardware detect, models, config, daemon
svrn doctor                                     # must come back clean
```

`svrn setup` will download models sized to *their* box. That is fine and
worth leaving alone — a local `fast` slot keeps completions and small
edits off the network, and the anchor still takes the heavy turns.

### When their box can't hold a model at all

An IoT device, a small VM, a machine you don't want carrying weights: set
it up as a **terminal** instead. It is a full member — mesh key, gossip,
pooled knowledge, ledger — that simply holds nothing and routes every
turn *and every embedding* to the anchor.

```sh
svrn setup --terminal http://<anchor>:9741   # downloads nothing
svrn mesh join "sovereign://join/cwth-…"     # the same link as any member
svrn daemon
```

**Chat works before they join.** The terminal forwards any model name it
cannot place itself to its entry node, which resolves the name and
serves it — so `svrn setup --terminal` alone is enough to get answers.
(This was not true until 2026-08-30: chat resolved against advertised
manifests first, the terminal honestly advertises none of its own, and
the anchor is not a peer until the join, so every turn 503'd.)

Joining is still worth doing, for what membership actually buys: gossip,
pooled knowledge, the contribution ledger, and — once the anchor is a
peer — a route that survives the anchor swapping addresses.

One thing a terminal cannot do at all: honour a `local_only` request.
Nothing runs on that machine, so the only place a turn can execute is
another host, which is the boundary `local_only` exists to defend. It
refuses and says so, rather than quietly forwarding.

`--terminal` refuses rather than writing a config it can't stand behind:
it asks the anchor's `/status` for the embed model id (which decides the
vector space this node's corpora land in) and drives one real completion
through it, printing which model answered. A config with no served turn
behind it is not a working setup.

Their editor still points at their **own** `localhost:9741` — §3 below is
unchanged. The daemon there advertises no models of its own — and no embed
model either, so the collaborative-ingestion planner never partitions chunks
onto a node that would only proxy them back here. `svrn mesh status` on the
anchor shows them as a member holding nothing, and no peer will ever route work
*to* them.

On the terminal itself, `svrn doctor` and `svrn mesh status` both say
`terminal` and name the entry node. That line matters: an empty model lineup
looks identical on a terminal and on a holder whose GGUFs failed to load, and
only one of those wants fixing.

What they give up: nothing runs locally, so there is no offline mode and
no local `fast` slot — every completion is a network hop. Ingest works
(chunks embed over HTTP against the anchor) but is slower than on a node
with its own embed slot.

**On the anchor**, read the invite:

```sh
svrn mesh status
```

Hand them the **`join link:`** line, not the bare `join key:`. The link
already carries the iroh dial string, so it works across networks with
no hand-editing; the bare key relies on mDNS finding you on a shared
LAN. ([Join a mesh](./JOIN_A_MESH.md) documents the older
`?relay=<ip>:9742` form — you do not need it if you pass the printed
link.)

**Back on their machine:**

```sh
svrn mesh join "sovereign://join/cwth-…?name=…&iroh=…"
svrn mesh status        # wait for [N/N online]
svrn mesh transport     # each peer's live path: direct / relayed / mixed
```

## 3 — Point their editor at their own daemon

Not at the anchor. Their local daemon does the routing.

```
base_url:  http://localhost:9741/v1
api_key:   anything (loopback callers need no token)
model:     primary   (or fast)
```

That is the whole configuration for anything that takes a `base_url` —
Zed, Continue, Cline, aider, opencode, the `openai` SDK. If their tool
speaks Ollama instead, the same port answers `/api/chat` and `/api/tags`
natively. [Using this with the tools you already
run](./INTEROP.md) has the full socket table, and
[INTEGRATION_SURFACES.md](./INTEGRATION_SURFACES.md) says which of those
are contracts rather than visible internals.

## Verify you have it

From a member machine, not the anchor:

```sh
curl -s localhost:9741/v1/models | grep -o '"id":"[^"]*"' | head
svrn chat
> Write a Rust function that reverses a string in place.
```

The claim is proven when a member with no 27B locally gets an answer
from one. On their side, `svrn mesh balance` shows the contribution
ledger. On the anchor, the per-peer serve count is on the status endpoint, not
in `svrn mesh status`:

```sh
curl -s localhost:9741/status | jq '.inference.peer_requests'
# [{"name":"RuggedFox","active":0,"served_total":2,"last_request_at":…}]
```

`served_total` climbing for a teammate's node is the anchor doing their
work.

## Serving faithfully

An anchor serves other people's tools, so it is worth knowing what the
daemon changes about a turn before it reaches the model. Two switches govern
all of it (`commonwealth/crates/commonwealth-api/src/turn_fidelity.rs`);
both are documented per-flag in [ENV_FLAGS.md](./ENV_FLAGS.md).

- **`SOVEREIGN_FRONTDOOR_AUTO_ALLOWLIST`** (default **off**) — when on,
  URLs and evidence handles seen in `role: tool` messages become sampler
  constraints. Right for retrieval synthesis, wrong for a general coding
  client. Leave it off on an anchor.
- **`SOVEREIGN_FRONTDOOR_RESHAPE`** (default **on**) — the runtime
  nudges and response canonicalizers. All of them key on the
  Codex/opencode contract, so most clients never trip them. Set `0` if a
  teammate reports the model saying something they did not prompt.

If someone asks whether this is "just llama.cpp with extra steps": the
inference is the same embedded llama.cpp, in-process, no proxy hop. The
honest differences are the two above, the one-turn-per-slot
serialization, and the queue shed — all named here, all switchable
except the serialization.

## When it breaks

- **`503` with `local_queue_full`** — the anchor is busy and shed rather
  than queued. Set `SOVEREIGN_MAX_QUEUE_WAIT_SECS=0` on the anchor
  (above). The body carries `Retry-After` and an OpenAI-shaped
  `error.code`, so a client can tell backpressure from a fault.
- **A member's requests never reach the anchor** — they are probably
  naming a concrete model id instead of `primary`/`fast`. Only the
  aliases are mesh-routable.
- **`401` on a guest link that worked an hour ago** — grants are held in
  memory. A daemon restart drops every outstanding grant. Re-mint.
- **Join hangs** — you handed out the bare key on a network where mDNS
  cannot cross. Hand out the `join link:` line instead.
- **Two members show one endpoint key** — `svrn mesh forget-member
  <node>` is the repair; see [join a mesh](./JOIN_A_MESH.md#when-it-breaks).
- Anything else: `svrn doctor` on both machines, then the
  [troubleshooting guide](../sovereign/docs/TROUBLESHOOTING.md).

## Related

- [Start the daemon](./START_THE_DAEMON.md) — install, setup, supervision.
- [Join a mesh](./JOIN_A_MESH.md) — ports, keys, relays, leaving.
- [Two-node quickstart](./TWO_NODE_QUICKSTART.md) — the same mesh, but
  federating *knowledge* rather than inference.
- [Threat model](./THREAT_MODEL.md) — what listens where, and why
  `:9742` never faces the internet.

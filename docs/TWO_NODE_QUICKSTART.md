# Two-Node Quickstart: a cited answer from a corpus that never leaves its machine

The federation claim, made concrete in one sitting: two machines, a
knowledge corpus that lives on only one of them, and a sourced answer on
the other — where what crossed the network was retrieval snippets, never
the corpus itself.

At the end you'll have:

- **Node A (founder)** hosting a corpus.
- **Node B (joiner)** asking a question and getting an answer whose
  `[Source: …]` citations draw on A's corpus, served over the mesh.
- A contribution-ledger entry on A recording that it served B's query —
  and the corpus bytes still only on A.

## Before you start

- Two machines on the same network, or both on one tailnet
  (Tailscale/Headscale). Between them: **TCP 9742** (mesh internal API —
  gossip and knowledge fan-out) and, on a shared LAN, **UDP 5353**
  (mDNS discovery). The client API (`:9741`) stays loopback unless you
  deliberately expose it.
- On each machine: install and run `sovereign setup` (downloads a
  primary + fast + embedding model for your hardware). The machine that
  *asks* does the synthesis, so it needs a real primary model (~2.5 GB
  at the CPU-only floor); the machine that only *hosts* knowledge works
  with the embedding model and its corpus.
- One thing worth knowing up front: the first daemon boot after `setup`
  quietly creates a **solo mesh** on that machine. That's why step 2
  reads the existing mesh rather than creating one — `sovereign mesh
  create` on an already-set-up machine errors with "a mesh already
  exists" (the fix is `rotate`, not `create`).

## 1 — Bring up both daemons

On each machine:

```sh
sovereign daemon start
sovereign mesh status     # each machine shows its own solo mesh [1/1 online]
```

## 2 — Read the invite on node A

```sh
sovereign mesh status     # prints the join key: cwth-XXXX-XXXX-XXXX
# want a fresh key (e.g. the old one leaked)? sovereign mesh rotate
```

## 3 — Join from node B

Same LAN (mDNS finds A automatically):

```sh
sovereign mesh join cwth-XXXX-XXXX-XXXX
```

Across networks, or on WiFi with client isolation, mDNS can't see A —
hand the join an explicit relay to A's internal port instead:

```sh
sovereign mesh join "sovereign://join/cwth-XXXX-XXXX-XXXX?relay=<node-A-ip>:9742"
```

Then on either machine:

```sh
sovereign mesh status     # wait for [2/2 online]
```

Connectivity details and firewall troubleshooting live in
[`commonwealth/docs/getting-started.md`](../commonwealth/docs/getting-started.md).

## 4 — Give A knowledge that B doesn't have

On node A:

```sh
sovereign corpus install sep      # Stanford Encyclopedia of Philosophy, ~0.5 GB
```

`sep` is the deliberate choice here, because its recipe encodes the
custody story this quickstart demonstrates:

- `query_sharing = true` — mesh peers may run federated searches
  against A's copy and receive cited snippets (fair use).
- `mesh_sharing = false` — byte-level replication of the index to
  peers is refused. The corpus never leaves A.

Those are two independently enforced gates, not one flag: fan-out
advertising filters on `query_sharing`
(`sovereign-mesh/src/capabilities.rs`), while the replication and
storage-snapshot paths filter on `mesh_sharing`. A corpus can be
queryable-but-never-copied (sep), or fully private (both `false`, or
`scope = "local"` to keep it off-mesh entirely).

(If `sep` isn't fetchable from your network, any corpus works for the
mechanics — `sovereign corpus list` shows the catalog; the `gutenberg`
catalog plus one `gutenberg-<id>` work is the fastest public-domain
alternative.)

## 5 — Ask from node B

On node B, which hosts nothing:

```sh
sovereign chat
> Is free will compatible with determinism?
```

What happens under the hood: B embeds the query locally, finds no local
corpus, and its daemon fans out to A's `/internal/knowledge/search`
(3-second per-peer budget). A returns scored chunks — text snippets
with corpus and chunk ids, not files. B merges them into its retrieval
context, synthesizes locally, and the grounding gate verifies the
answer against that evidence before showing it. The `[Source: …]`
header names the corpus that answered, attributed to the peer that
served it.

## What just happened — the custody ledger

- **Chunks moved; the corpus didn't.** The knowledge-search response
  carries `content`/`title`/`corpus_id`/`chunk_id` per hit. There is no
  route that streams corpus files in the query path; replication is a
  separate, `mesh_sharing`-gated mechanism that `sep` opts out of.
- **The serve was recorded.** A emits one `KnowledgeQueryServed` ledger
  event per contributing corpus, stamped with B's node id — visible in
  the contribution ledger (`sovereign mesh balance`).
- **Failure degrades, never breaks.** If A is offline it is excluded
  from the fan-out plan; transport errors never propagate — B just
  answers from whatever it can reach.

## Verify it's real (glassbox)

The raw fan-out, without the chat layer, from node B:

```sh
curl -s -X POST http://127.0.0.1:9741/v1/knowledge/search \
  -H 'content-type: application/json' \
  -d '{"query": "compatibilism and determinism", "corpora": ["sep"]}'
```

Look for: non-empty `results[]` with `corpus_id: "sep"`,
`corpora_searched` containing `"sep"`, and each hit's
`metadata.peer_name` naming node A. This exact flow — two daemons, sep
hosted only on the founder, cited results on the joiner, ledger event on
the founder — is pinned by the integration suite
(`sovereign/crates/sovereign-mesh/tests/knowledge_fanout_e2e.rs`), and
the queryable-but-never-copied split by
`tests/local_only_corpus_locality.rs` and `tests/storage_snapshot_e2e.rs`.

## Appendix: two daemons on ONE machine

Possible, with two caveats the two-machine path doesn't have. Each
daemon needs its own config (distinct ports and data dir):

```toml
# node-b.toml
[daemon]
client_port = 9743
internal_port = 9744
client_bind = "127.0.0.1"
[data]
dir = "/tmp/svrn-node-b"
```

and because the `sovereign mesh join` CLI talks to the daemon on
`:9741`, the second daemon joins via its own client port directly:

```sh
sovereign daemon run --config node-b.toml &
curl -s -X POST http://127.0.0.1:9743/v1/mesh/join \
  -H 'content-type: application/json' \
  -d '{"key_or_url": "sovereign://join/cwth-XXXX-XXXX-XXXX?relay=127.0.0.1:9742", "node_name": "node-b"}'
```

Second caveat: there is no mDNS-off switch yet, so two bare daemons on
one machine will also discover any real mesh on your LAN. The
multi-process soak harness (`scripts/mesh-soak.sh`) solves this with a
rootless network namespace on Linux; treat the one-machine form as a
dev convenience, not the demo.

## Troubleshooting

- **"A mesh already exists"** on `mesh create` — expected after
  `setup`; read the key with `sovereign mesh status` or mint a new one
  with `sovereign mesh rotate`.
- **Join hangs on shared WiFi** — client isolation is blocking mDNS;
  use the explicit `?relay=<ip>:9742` join form.
- **macOS firewall prompt** for the daemon listening on `0.0.0.0:9742` —
  allow it; that's the mesh internal port (see
  [`docs/THREAT_MODEL.md`](./THREAT_MODEL.md) for exactly what listens
  where and why).
- **503 on a query** — a peer left mid-plan; plans rebalance within
  ~5–15 seconds, retry.

# RuggedFox results — terminal node, two-machine run, 2026-08-31

Against `FOX_BRIEF-2026-08-31-terminal.md`. Commit `68cca3de4`, `3405fb64f` an
ancestor. Raw log: `RESULTS-2026-08-31-fox-terminal.raw.log`.

Terminal node id: `f1f2589f3f39bdd661e7bfce8d3a2c5f` (roster name `fedora`)
Terminal dial: `e5ab560903281007cc15578df76fa31edd873d485fb25b3ac0fc0b42b4c29af4@https://usw1-1.relay.n0.iroh.link./,69.181.167.209:36377,100.115.12.21:36377,192.168.1.13:36377`

| Bar | Verdict | Observed |
|---|---|---|
| F0 parity | pass | `68cca3de4`, has `3405fb64f`, MAC online |
| F1 onboarding | pass | exit 0; discovered `Alexs-MacBook-Pro-2`; turn answered by `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL` |
| F1 config | pass | `entry_node = "37f17554b6c4ff292af4844ad4dbc43c"`, no `entry =` address key |
| F2 start | pass | `/status` 200 in 2s |
| F3 advertises nothing | pass | `models = []` |
| F4 mesh alias | pass | `primary` present (~2s) |
| F5 class + binding | pass | `node_class: terminal`, `entry_node: 37f17554b6c4ff292af4844ad4dbc43c` |
| F6 real turn | **see below** | HTTP 200 `"ready"` — but `model: "primary @ peer RuggedFox"` |
| F7 embedding | **pass** | 1024 dims, resolved to `Alexs-MacBook-Pro-2` |
| F8 local_only refused | pass (main bar) | HTTP 503, not served |
| F8 gate name | **differs** | `gate="privacy_local_only"` (`local_has=false`), not `forwarder_cannot_serve_local_only` |
| F9 transport | pass | `endpoint=http://127.0.0.1:39941/v1` — loopback iroh bridge; no plaintext path |

## F1 could not run as written, and that is itself a finding

`daemon_is_listening()` (`setup_cmd/terminal.rs:482`) probes a hardcoded
`http://127.0.0.1:9741/v1/models`. It never consults the `HOME`-redirected
config, so the brief's sandbox mechanism does not cover it and setup refuses on
any host already running a node. Run under operator approval with the real
daemon stopped for 2m54s; it was restored with byte-identical `RUST_LOG` and
RuggedFox is back online.

This is also why F1 had never been exercised: `scripts/terminal-e2e.sh`
hand-writes the config and calls `mesh join` (lines 112-132), noting "the
product path never types this."

## F6 — the binding is not the decider for chat

The turn was served by **RuggedFox, this same machine**, not by the bound entry
node. Reproduced twice, deterministic:

    routing decision  path=NamedModel verdict=named_peer:RuggedFox
    routing outcome   served_by=peer:RuggedFox total_ms=Some(369.846168)

Mechanism, from the manifests fetched immediately before:

    peer=RuggedFox              rtt_ms=9   locality=Near
    peer=Alexs-MacBook-Pro-2    rtt_ms=52  locality=Far

Chat resolves through the advertised manifest and prefers `Near`, never
consulting `EntryNodeEndpoint` — the bypass the brief predicted in F7's note.
Corroborated from RuggedFox's own side, `/status.inference.peer_requests`:

    {"node_id":"node-f1f2589f3f39bdd6","name":"fedora","active":0,
     "served_total":2,"last_request_at":1788216729}

So the literal F6 bar (200 + real content) is met while the claim it exists to
test — a turn leaving this host for its bound entry node — is falsified on the
chat path.

**The real finding is a split decider (ARCH principle 8, one decider one name).**
Two subsystems answer "where does a turn go" differently:

- embeddings -> `EntryNodeEndpoint` -> the bound node (F7, MAC)
- chat -> `provider_for_peer` -> nearest advertising peer (F6, RuggedFox)

`/v1/mesh/status` reports the binding, and setup tells the user "The bind is
Alexs-MacBook-Pro-2's mesh identity." On the most common path that is not true.
MAC's co-located run could not see this: with only one holder, Near and bound
are the same node.

## F7 is the off-box proof

1024 dims, and at the request timestamp:

    22:52:09.514990 DEBUG terminal: resolved entry node
      entry_node=node-37f17554b6c4ff29 peer=Alexs-MacBook-Pro-2
      endpoint=http://127.0.0.1:39941/v1

Loopback endpoint, far end MAC — the encrypted iroh path. Bytes genuinely left
this host for the bound entry node.

## F8 — refused correctly, under a different gate

`privacy_local_only` fires first because `primary` locates as `Peer` (both
RuggedFox and MAC advertise it). `forwarder_cannot_serve_local_only`
(`peer_inference.rs:2503`) guards the `Local`-on-`ForwardsOffBox` arm, which is
shadowed whenever a peer advertises the named model. Both defend the same
invariant and the refusal is correct; the brief's predicted gate appears
unreachable in this configuration.

## Two instrument defects in the brief itself

Both would have produced false verdicts, so they are worth fixing before the
next run:

1. `RUST_LOG=info,transport=debug` raises only `transport`. Both `gate=` sites
   are `tracing::debug!` on `sovereign_mesh`, so F8's gate check reads empty on
   a correct run. Needs `sovereign_mesh=debug`.
2. `grep -o 'gate=[a-z_]*'` can never match — the field renders quoted,
   `gate="privacy_local_only"`.

## For MAC's corroboration

- The field path is `/status.inference.peer_requests`, **not** top-level
  `/status.peer_requests` as the baseline records. Reading the top level returns
  nothing and would book a false negative.
- Corroborate against **F7 (embeddings) and F1**, not F6. MAC served the F1
  setup turn and both F7 embeddings; it did **not** serve F6.
- MAC advertises more than the baseline recorded — `/v1/models` gossiped
  `Qwen3.5-4B-UD-MTP-Q6_K_XL`, `Qwen3.6-35B-A3B-MTP-UD-Q6_K`,
  `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`, `Qwopus3.5-4B-v3-MTP-Q8_0`, plus the
  `fast`/`primary` aliases. F1's turn was answered by the 35B-A3B, not the
  `Qwopus3.5-4B` the baseline named.

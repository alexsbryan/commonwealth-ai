# mesh-llm: why we are not adopting it, what to take, and how to build for extension

Operator request 2026-09-17, after asking whether we should "just adopt"
mesh-llm. The deciding criterion, in the operator's words: "It needs to be
extensible for what I want to do -- I can't have some sprawling 700k opinionated
ball that I'm just going to have to vendor to get anything done with."
Read-only due diligence; no product code lands from this report.

Sources: the mesh-llm repository (https://github.com/Mesh-LLM/mesh-llm, Apache-2.0),
cloned 2026-09-17 at commit `37fe1fa24`; citations into it are written
`mesh-llm:path:line`. GitHub API data for the project (issues, PRs, CI runs,
releases, forks) fetched the same day. The Block Engineering post "Buzz: Sharing
compute powered by MeshLLM" (2026-07-27). This repo at `ralph/domains-campaign`
HEAD, cited `path:line`; the notes store (ids in place).

Verification status: claims marked **(checked)** were re-read in code by the
author. Everything else comes from four review passes (inference, mesh and
trust, project health, and our own extensibility) and carries a citation so it
can be checked. Nothing in either codebase was built or run for this report.

## BLUF

**Do not adopt mesh-llm, as a whole or as a library.** It fails the
extensibility criterion on every axis we measured: its mesh cannot be separated
from its inference engine, its plugin seam stops at transport, its engine is a
deep llama.cpp fork that cannot serve our verification layer, and it grows too
fast to depend on. Its defaults also contradict ours (an unconditional GitHub
release check, maintainer-namespaced relays, admission off by default).

**Three ideas are worth taking**: layer packages (a stage serves without the
whole model), owner-signed node certificates, and a backend-neutral host that
loads a separately shipped engine. Its decision to abandon ggml-rpc tensor
split in May is also evidence about our own weakest subsystem.

**The more useful lesson is about us.** Our mesh substrate (`commonwealth-*`)
is light and gated, which mesh-llm never achieved. But the layer above it
repeats three of mesh-llm's failures: `sovereign-mesh` drags in llama.cpp and
ort through one function call and one dead dependency, an app cannot extend
the gossiped member record or register an ALPN, and our peer wire contracts
carry no version. And one of our gaps is worse than theirs in a way that
matters now: the internal mesh API binds `0.0.0.0`, the iroh internal ALPN
forwards any dialer, and the federated search handler does not check
`query_sharing`. Section 4 turns each lesson into a structural rule with a
guard.

## 1. The adoption verdict

| Check | Finding | Evidence |
|---|---|---|
| Can the mesh be used without the engine? | No. Gossip, membership, admission and the iroh endpoint live only in `mesh-llm-host-runtime` (270k raw lines, 40% of the Rust). Its Skippy engine dependencies are non-optional, and `src/mesh` references `crate::inference` on 272 lines. The planned crate split (`docs/design/CRATE_DECOMPOSITION.md`) exists only as a proposal. | mesh-llm:crates/mesh-llm-host-runtime/Cargo.toml:55-66 **(checked)** |
| Is the plugin API a sufficient seam? | Partly. Published and versioned (protocol 3), but the version must match exactly. It gives no verified sender identity to plugins, no way to add fields to gossiped peer records, and no ALPN registration. Three of our six mesh apps would fit (rail journal, lease plane, work atlas); custody search, capability ads and loopback app exposure would need host-runtime patches. The org's own out-of-tree plugins pin git branches that no longer exist. | mesh-llm:crates/mesh-llm-host-runtime/src/plugin/runtime.rs:313-320; src/mesh/plugin_streams.rs:99-101 |
| Can its engine carry our verification layer? | No. `logprobs` are rejected, embeddings and rerank are "not an implemented product mode", there is no FIM, no idle unload, no per-request local-only routing. The engine is upstream llama.cpp plus 57 hand-written patches (44,965 lines) and 77 generated patches rewriting every model builder, loaded once per process. Local multi-GPU `tensor_split` is refused. | mesh-llm:crates/skippy-server/src/frontend/request.rs:637-650; crates/mesh-llm-config/src/wiring_status.rs:1775-1783; third_party/llama.cpp/patches/; crates/skippy-ffi/src/dynamic.rs:20; crates/mesh-llm-host-runtime/src/inference/skippy/resolver/support.rs:97-98 **(all checked)** |
| Is it stable enough to depend on? | No. 672,015 lines of Rust (outside `third_party`) after 6.5 months, deleting 18-56 lines per 100 added. The split-serving stage protocol is at generation 10 with a written no-backward-compatibility rule; release notes never flag breaks. Block's Buzz, its main consumer, pins `mesh-llm-host-runtime` to git tag `v0.76.0-rc9`, which is the vendoring we want to avoid. | mesh-llm:crates/skippy-protocol/src/validation.rs:9; block/buzz desktop/src-tauri/Cargo.toml:114-115 **(checked)** |
| Do its defaults fit a private, no-phone-home product? | No. Every start queries `api.github.com` for releases with no off switch, including when embedded. Private meshes use relays under `*.relay.michaelneale.mesh-llm.iroh.link` unless hidden flags are passed. The default trust policy is `Off`, and its own docs call invite tokens "not private credentials". Hostname and GPU inventory are gossiped to every peer. The local API has no authentication and sends CORS `*`. | mesh-llm:crates/mesh-llm-host-runtime/src/runtime/run_auto.rs:281-290; src/mesh/connections.rs:178-188; crates/mesh-llm-identity/src/ownership.rs:88-98 **(checked)**; docs/LOGGING.md:111-117 |
| Governance | Three people merge 98.5% of PRs; 46% of maintainer PRs merged with no human review; no SECURITY.md or advisories; a CERT-PL report was fixed under a `chore:` title and shipped with empty release notes; 23% of pushes to main in 30 days had a failing CI lane. Effectively a Block/Spiral side project with one downstream (Buzz); goose removed its mesh UI on 2026-06-24. | GitHub API data and the project's own issue #1005 / PR #1007 |

The one integration that satisfies the criterion is treating a running mesh-llm
as an OpenAI endpoint behind our `kind="remote"` engine, for plain generation.
It costs nothing and delivers little, since our grounding judge needs
probabilities it does not return.

## 2. Ideas worth taking

**Layer packages.** A model is published as `shared/{metadata,embeddings,output}.gguf`
plus one GGUF per layer, each with a SHA-256, and a pipeline stage fetches only
the pieces it serves (mesh-llm:docs/specs/layer-package-repos.md:52-99, 812-830).
Our ggml-rpc path is host-loads-all: the host streams each worker's weights at
load, which is the source of the `send()` deadlock our warm cache works around
(note 94eff39e). Whatever replaces tensor split here should let a node hold
only its shard.

**Their exit from ggml-rpc.** mesh-llm replaced llama-server plus ggml-rpc with
layer-stage pipeline parallelism on 2026-05-05 (#422). Their published cost is
about two network round trips per decoded token: a Metal + CUDA split across a
20 ms link ran 17 tok/s against 77 on one node
(mesh-llm:docs/skippy/WAN_SPLIT_PERF.md:48-63). Upstream's own README calls RPC
"fragile and insecure", and our vendored copy carries a remote-code-execution
bug fixed upstream on 2026-09-16. Tensor split over ggml-rpc is the weakest
part of our mesh and needs a redesign decision. The price mesh-llm paid, a
permanent patch queue on llama.cpp core, is the wrong answer for us.

**Owner-signed node certificates.** An owner key signs short-lived (7-day)
certificates binding owner to node id; a trust policy can then require owned
nodes or an owner allowlist (mesh-llm:crates/mesh-llm-identity/src/ownership.rs).
That is the shape of a fix for our two-identities-per-machine incidents (a
random `NodeId` beside the Ed25519 pubkey). Do better than they did:
revocation in mesh-llm is local to each node and never gossiped.

**A backend-neutral host with a separately shipped engine.** Their host binary
knows nothing about GPU backends; it loads exactly one native runtime directory
per install, chosen per machine (Metal, Vulkan, ROCm, CUDA, CPU), against an
exact ABI (mesh-llm:docs/design/NATIVE_RUNTIMES.md). Our release legs compile
llama.cpp into the daemon for every target. The same decoupling is available to
us without a patch queue by running upstream's prebuilt server as a sidecar;
see [LLAMA_SERVER_ADOPTION.md](LLAMA_SERVER_ADOPTION.md).

Smaller ones: a certified-architecture roster that makes splits fail closed for
untested model families (sound idea, although their certification compares one
argmax token); a unified-memory heuristic for Strix Halo that treats small VRAM
plus large GTT as UMA (mesh-llm:crates/mesh-llm-system/src/hardware/parsers.rs:115-158,
which a large BIOS carve-out defeats); an mDNS discovery mode that turns off
relays and public discovery with one switch; prefix-hash affinity routing to
keep a conversation on the node holding its KV.

## 3. Their failures, and where we repeat them

| mesh-llm failure | Our state | Evidence |
|---|---|---|
| Mesh fused with the engine in one crate | **Substrate good, host layer same failure.** The nine `commonwealth-*` crates have closures of 42-329 crates with no llama.cpp, ort, lance or tree-sitter, enforced by `boundary-gate`'s `[[package]] commonwealth` rules. `sovereign-mesh` and `sovereign-daemon` (about 905 crates each) carry all of them. For `sovereign-mesh` the engine edge is one call, `sovereign_inference::embedded::local_gpu_total_vram_gb()`, and the `sovereign-gliner` edge has zero uses. | `sovereign/crates/sovereign-mesh/src/capabilities.rs:77` **(checked)**; zero `sovereign_gliner` references in `sovereign-mesh/src` **(checked)**; `quality/ARCH_LAYERS.toml:1018-1088` |
| Plugin seam stops at transport | **Partial.** HTTP apps on the app/media ALPNs receive verified `X-Mesh-*` identity headers, with spoofed headers stripped. But `MemberRecord` has no extension field, `OriginKind` is closed, the ALPN set is an if-chain in both acceptors, and first-party apps (knowledge fan-out, work atlas) are wired into the core rather than built on the seam a third party would use. | `commonwealth/crates/commonwealth-core/src/mesh/mod.rs:200-232` **(checked)**; `sovereign-mesh/src/iroh_access.rs:434-580`; `commonwealth-rails/src/acceptor.rs:86-150` |
| Exact-match versions, breaks unflagged | **Partial.** MCP negotiates versions, recipes carry a max schema version with old fixtures, and OICP gates on features rather than version strings. But gossip and `/internal/*` bodies carry no version, closed enums lack `serde(other)`, and a rail op's signature covers the act as the receiving build re-serializes it, so adding a field would make old nodes report bad signatures. | `commonwealth-core/src/mesh/wire.rs:73-100`; `commonwealth-rail-core/src/admit.rs:331,545` |
| Unconditional phone-home, public-infrastructure defaults | **Partial.** No telemetry and no automatic update check. But the Wikipedia freshness poller defaults on, iroh turns on for any mesh participant and then uses n0 relays and discovery unless configured, and the search tool falls back to DuckDuckGo. "Nothing phones home" is prose in README and the architecture tour; the F26 egress census counts reqwest construction sites only. | `sovereign-contracts/src/setup_config.rs:1731-1733` **(checked)**; `sovereign-mesh/src/iroh_access.rs:198-208` **(checked)**; `sovereign-core/tests/main/f26_egress_census.rs` |
| Admission open by default | **Same failure, on `:9742`.** The internal API binds `0.0.0.0` by default. The iroh internal ALPN forwards any dialer (the code names this "a known open edge"). The peer admission layer passes any request without an `x-node-id` header straight through, and the federated search handler does not read `query_sharing`. The review pass also lists model load/unload and corpus install as reachable; that was not exercised. Note 135f81ef recorded the unauthenticated bind on 2026-08-04; the fix has not landed. | `sovereign-contracts/src/setup_config.rs:1684-1688`; `sovereign-mesh/src/iroh_access.rs:420-442`; `sovereign-serving-host/src/admission.rs:490-500`; `sovereign-api/src/routes_internal/knowledge.rs` **(all checked)** |
| Docs claiming what the code lacks | **Same failure.** `SYSTEM_OVERVIEW.md` §5 describes three-phase digest/delta gossip (the code pushes full snapshots), UDP latency probing with a `CWLP` magic (absent), and tensor-split RPC as "raw TCP" (an iroh `cwth/rpc/0` class exists). `THREAT_MODEL.md` claims per-handler loopback guards on admin routes and `query_sharing` gating on federated reads. `SYSTEM_OVERVIEW.md` and `corpus-engine/src/index/mod.rs` say keyword search uses Tantivy; it uses Lance's inverted index. | `sovereign/SYSTEM_OVERVIEW.md:212,671,874` **(checked)**, `:4678-4683`; `docs/THREAT_MODEL.md` |
| Sprawl | **Better, not immune.** 539k code lines excluding tests; Rust deletes 23.9 lines per 100 added since 2026-03-31. `sovereign-cli-llm` is 17.3% of code and about half of it is eval and bench harness, but it is a leaf that no mesh crate depends on. | `quality/baselines/lines.tsv`; `git log --numstat` |
| Twins | **Same failure.** Three app registries (one, `sovereign-meshapp-registry`, has no production caller that ever fills its port map), three `mesh.json` shapes, and a `commonwealth-rails` gossip/join twin that already diverged from `sovereign-mesh` (fanout 3 vs 2). None has a `quality/twin-plants.toml` row. | note be0cd654 |
| Correctness certification that cannot fail | **Analogous.** `engine_conformance.rs` is generic over the engine trait but has only ever run against fakes. | `sovereign/crates/sovereign-inference/src/engine_conformance.rs` |

What we do that mesh-llm does not, and should keep: package liftability with
`[[forbid]]` tables enforced by `boundary-gate` and `layer-gate`; corpus-mcp's
`tests/no_inference_stack.rs`, which fails the build if llama, ort or iroh enter
its dependency tree; physical lift proofs that build a package outside the repo
(`scripts/serving-lift.sh`, `cw-rails-lift.sh`, `cw-work-lift.sh`); the env-flag
registry and `env-gate`; recipe back-compat fixtures; MCP version negotiation;
fail-closed encrypted-mesh mode with signed dial info.

## 4. Building for extension

Each rule below names the invariant, the guard that makes it structural, the
existing surface it extends, and the smallest first step. They are ordered by
leverage, not by urgency (see §5 for order).

### Rule 1: the substrate stays light, and a test proves it

The `commonwealth` package boundary already keeps workspace crates out; it does
not see third-party crates. Extend it.

- Guard: add a forbidden-externals list (`llama-cpp-sys-4`, `ort`, `lance`,
  `lancedb`, `tree-sitter`) to the `commonwealth` package in `boundary-gate`,
  or generalize `corpus-mcp/tests/no_inference_stack.rs` into a table-driven
  test over every liftable package.
- First step: move the VRAM probe out of `sovereign-mesh` (a
  `commonwealth-discovery` hardware probe or a port), delete the dead
  `sovereign-gliner` edge, and make `sovereign-tools` a dev-dependency. Then
  `cargo tree -p sovereign-mesh -e normal` should show no llama.cpp or ort;
  that command becomes the test.
- Wire the existing lift scripts into the weekly workflow so the proof is not
  manual.

### Rule 2: one extension seam, and first-party apps use it

mesh-llm's plugin API is weak because almost nothing of its own uses it. A seam
only stays sufficient if our own apps are its customers.

- Pick the one app surface (`commonwealth_media::apps::PublishedApps` is the
  live one and the documented one in `docs/PUBLISH_AN_APP.md`), delete
  `sovereign-meshapp-registry`, and register the remaining twins in
  `quality/twin-plants.toml`.
- Move knowledge fan-out and the work atlas onto that seam as the proof that it
  is sufficient; anything they need that it lacks is exactly what a third party
  would lack.
- Guard: a census test over the `/internal` router that fails when an
  app-specific route is added outside a declared core list.

### Rule 3: open the three doors mesh-llm left closed

- **Member-record extensions.** Add a namespaced, size-capped extension map to
  `MemberRecord` (or `NodeCapabilities`), covered by the record's signature,
  preserved and relayed verbatim by nodes that do not understand a key. Corpus
  and hardware advertisements become its first users.
- **ALPN registration.** Replace the acceptor if-chains in `sovereign-mesh` and
  `commonwealth-rails` with one registered `ALPN → forward` table in
  `commonwealth-transport`, so a new traffic class is a table row, not an edit
  to two daemons.
- **Verified identity everywhere an app listens.** HTTP apps already receive
  verified identity headers; extend the same guarantee to stream-based apps, so
  no app ever trusts a self-asserted id (today knowledge fan-out trusts
  `X-Node-Id`, note 37e1e3dd).

### Rule 4: version what crosses the wire

- Add a wire version or build stamp to `MeshWire` / `MemberRecord`, as the
  backlog already asks (note c61d9a8c), and add `serde(other)` arms to the
  closed enums that travel.
- Sign rail ops over the canonical bytes as received, not as re-serialized, so
  adding a field does not break old verifiers.
- Guard: a golden old-payload fixture per gossip body, rail body and
  `/internal` request body, run in the normal suite. `api-gate` cannot see
  serde attributes, so fixtures are the gate.
- Keep OICP's rule as the house rule: negotiate on features, never on version
  strings.

### Rule 5: admission is deny-by-default

- Split the internal listener the way the guest listener was already split: a
  join-only surface for non-members, a member check by dialer key on every
  other `/internal` route. The code comment at
  `sovereign-mesh/src/iroh_access.rs:426-433` already names this fix.
- Enforce `query_sharing` in the serving handler, not only in capability
  advertisement.
- Require the Ed25519 proof whenever a pubkey is presented, and turn strict
  gossip auth on by default.
- Guard: a table-driven test that dials every `/internal` route as a
  non-member and expects refusal except for join, generated from the router's
  own route list so a new route cannot skip it.

### Rule 6: egress is a gate, not a sentence

- Guard: a boot test that starts the daemon under a default config with a
  deny-all connector and asserts zero outbound connections for a fixed window;
  extend the F26 census to `reqwest::get(`, iroh, and mDNS.
- Defaults: iroh discovery and the Wikipedia poller off until the operator opts
  in, so the README's claim becomes true by construction.

### Rule 7: claims in contract docs are checkable

Correct the five false claims in §3 in the same commit as any code they
describe, and make the `SYSTEM_OVERVIEW.md` §5 and `THREAT_MODEL.md` claims
drift anchors or test-backed citations, so the next divergence fails a gate
instead of waiting for an audit.

### Rule 8: an engine is a replaceable process

The inference engine is the component most likely to be replaced, by upstream
progress if nothing else. The seam that makes it replaceable exists
(`kind="remote"`), but today it drops features silently. Make it honest (see
[LLAMA_SERVER_ADOPTION.md](LLAMA_SERVER_ADOPTION.md) §5 and Phase 0), and run
`engine_conformance` against real engines rather than fakes.

## 5. Order of work

1. **`:9742` admission (Rule 5).** A security gap on a default-open port; the
   design is already named in code.
2. **Vendored ggml-rpc use-after-free.** Five lines in `free_buffer`, or a
   vendor re-sync.
3. **Remote seam honesty (Rule 8).** Silent grounding fail-open today.
4. **Light substrate guard and the two-edge cut (Rule 1).** Cheap, and it turns
   an existing property into an enforced one.
5. **Member-record extensions and the ALPN table (Rule 3), with first-party
   apps moved onto the seam (Rule 2).**
6. **Wire versioning fixtures (Rule 4) and the boot egress test (Rule 6).**
7. **Doc corrections (Rule 7)**, riding along with whichever change touches
   each claim.
8. **Tensor-split redesign decision**, informed by layer packages and by the
   llama-server spike.

## 6. Not verified

- Anything run: neither mesh-llm nor our daemon was built or exercised for this
  report.
- That a non-member request to `:9742` succeeds end to end, and which
  `/internal` routes beyond federated search (model load/unload, corpus
  install) are reachable that way.
- That cutting the two `sovereign-mesh` edges clears llama.cpp and ort from its
  closure (manifest-graph reasoning, not a `cargo tree` run after the change).
- mesh-llm behaviour on Strix Halo, Windows native, or a mixed-version mesh.
- Whether Block or Spiral employs the maintainers other than Michael Neale, and
  who pays for the relays and public console.
- Public-mesh size (about 10-14 nodes on 2026-09-17, one live snapshot).

# Target architecture — the nouns and verbs

**What this is.** The destination of the
[noun-convergence program](./NOUN_CONVERGENCE.md): the architecture after
the refactor, organised around what the system is made of and what may be
done with it, rather than around where code lives. The program's terminal
test is that this document can one day be written **honestly** — generated
from the register and the graph, claiming only structure that exists,
rendering what is missing as a visible gap — because a few interacting
abstractions can be generated and spaghetti cannot. A system that needed
this document narrated by hand would have failed the program regardless of
any metric.

**What this is not.** Not a contract in the sense of
[`ARCH_PRINCIPLES.md`](../sovereign/ARCH_PRINCIPLES.md) §1.1.
[`SYSTEM_OVERVIEW.md`](../sovereign/SYSTEM_OVERVIEW.md) remains the record
of what IS, verifiable on the commit it appears in. When the program
completes, this replaces that file and the distinction disappears.

**Where the detail lives.** [`CONCEPTS.toml`](./CONCEPTS.toml) is the
register — canonical owner, totality rule, phase, and *today's shape with
citations* for every noun below. This document does not restate it. Each
noun here carries a status marker and a one-line totality; open the register
row for the evidence.

| Marker | Meaning |
|---|---|
| **`holds`** | true today, verifiable on this commit |
| **`partial`** | the mechanism exists; the invariant is not yet total |
| **`target`** | does not exist yet |

---

## 1. The thesis, as one invariant

> **Nothing reaches the user that did not come from Evidence, and no
> Evidence exists without an Origin and a Custody.**

That is the product claim — *an assistant that runs on your machine and
proves what it says* — expressed as a type invariant rather than a practice.
Today the same guarantee rests on 147 correct decisions about corpus
sharing, an untyped `metadata["provenance"]` channel, and a runtime gate
that re-derives what is citable. The refactor moves it from vigilance into
construction.

Three corollaries, each enforced by a type rather than a review:

1. **A model may only read from a seal.** Composition takes a sealed
   `EvidenceSet` and nothing else; there is no path from a corpus to a
   prompt that bypasses retrieval. *(`partial`)*
2. **Absence is a value, never a default.** An unwired capability returns
   `Unavailable(reason)`. Nothing returns an empty collection to mean "not
   configured". *(`target`)*
3. **Two numbers may be compared only if they are comparable.** A
   `Measurement` carries the fingerprint of the conditions that produced it,
   and the gate refuses to diff across fingerprints. *(`target`)*

---

## 2. The nouns

Generated from [`CONCEPTS.toml`](./CONCEPTS.toml). Every noun, its one-line
sense, its status marker, its declared owner and its totality rule are read
from the register — so this section cannot name a noun the register does not
carry, and cannot omit one it does. Both were true of the hand-written version
it replaces: it described a `Question` no register row backs, and never
mentioned five rows that exist.

<!-- BEGIN GENERATED register -->
**31 nouns.** 7 `holds`, 9 `partial`, 15 `target`; 24 are in the program, the rest are here for architectural completeness. Every row below is a `[[concept]]` in [`CONCEPTS.toml`](./CONCEPTS.toml) — the count, the markers and the owners are read from it, not typed here, so this section cannot claim a noun the register does not carry.

| noun | what it is | status | owner (declared) | phase | totality |
|---|---|---|---|---|---|
| **`Measurement`** | one comparable observation. | `target` | `sovereign-eval` | 1 | The comparability fingerprint IS the key — lane, corpus, model attribution, prompt version, sample cap, scorer version, every field required. |
| **`Baseline`** | the committed prior for one fingerprint. | `partial` | `sovereign-eval` | 1 | Stored under its fingerprint. |
| **`Attribution`** | which engine computed this text. | `target` | `sovereign-contracts` | 1 | ONE answer to 'which engine computed this text': model identity, build, quantization, serving host. |
| **`Verdict`** | the outcome of any check. | `target` | `sovereign-contracts` | 2 | Four states, one definition, workspace-wide: Passed \| Failed(Reason) \| CouldNotJudge(Reason) \| NeverRan(Reason). |
| **`Judgement`** | a verdict with its evidence and calibration. | `target` | `sovereign-contracts` | 2 | A Judgement is MINTABLE ONLY BY A CALIBRATED JUDGE — private constructor, the CalibrationReceipt pattern gym_judge already ships (a capability token only judge_calibration can mint; `untrusted()` exists and warns on every call). |
| **`WireMessage`** | one definition per daemon-boundary message. | `target` | `sovereign-wire` | 3 | One definition per daemon-boundary message. |
| **`SurfaceError`** | a structured error returned by a command, never a String. | `partial` | `sovereign-desktop` | 3 | A command returns a structured error, never a String. |
| **`BootstrapMode`** | how this install was brought up — Attach or Local, distinguishable by the compiler. | `partial` | `sovereign-desktop` | 3 | The sum type survives into AppState. |
| **`Origin`** | where a piece of knowledge came from. | `target` | `sovereign-contracts` | 4 | Every field non-optional, and the SOURCE IS A CLOSED SUM — Corpus{corpus, document, locator} \| Web{url, fetched_at} \| Attachment{asset, locator} \| ToolOutput{tool, call_hash} — plus served_by: Local \| Peer(node, name), and grain: Leaf \| Summary. |
| **`Custody`** | where a piece of content stands, carried by the chunk. | `partial` | `sovereign-contracts` | 4 | The chunk-level custody class — where this content stands (PublicWeb \| Personal \| Peer \| ...) — is a REQUIRED, TYPED field of Evidence, stamped at retrieval from the corpus's current policy. |
| **`SharingPolicy`** | what a *corpus* permits the mesh. | `target` | `corpus-engine` | 4 | The corpus-level answer to 'may peers query this / may the bytes move' — one policy TYPE with one resolution decider, declared by the Recipe, stamped into the index meta, surfaced by the registry. |
| **`Evidence`** | the retrieval unit, and the only thing a model may read. | `target` | `corpus-engine` | 4 | No public constructor, no Default, no public fields, and NO `Deserialize` — a derive that would put the back door straight back on. |
| **`EvidenceSet`** | a sealed body of evidence for one turn. | `target` | `sovereign-contracts` | 4 | TWO seals, both carried: the CHUNK SET composition read, and the CORPUS SCOPE retrieval was bound to. |
| **`Draft`** | generated text, not yet released. | `target` · landed 2026-08-20, rung nc-11-answer. | `kernel-types` | 4 | A Draft cannot be returned to a surface. |
| **`Answer`** | what the user sees. | `partial` · landed 2026-08-20, rung nc-11-answer. | `kernel-types` | 4 | Cannot exist without a Judgement. |
| **`Citation`** | a pointer into the sealed `EvidenceSet`. | `target` · landed 2026-08-20, rung nc-11-answer. | `kernel-types` | 4 | A Citation points INTO the sealed EvidenceSet — it cannot cite what the seal does not contain. |
| **`Capabilities`** | what this install can actually do. | `target` | `sovereign-core` | 5 | A declared, validated profile. |
| **`Step`** | one typed unit of the single dataflow substrate. | `partial` | `sovereign-workflow` | 5 | One dataflow substrate. |
| **`Artifact`** | the substrate's typed step output — content-addressed, cacheable. | `partial` | `sovereign-workflow` | 5 | The substrate's typed step output — content-addressed, cacheable. |
| **`Intent`** | what kind of ask it is. | `partial` | `sovereign-contracts` | 6 | The enum plus a data table — one row per intent declaring handler, slot, retrieval policy, gate surface, budget, labels. |
| **`Command`** | one CLI promise. | `partial` | `sovereign/docs/cli-contract.toml` | 6 | The contract is GENERATIVE. |
| **`Record`** | an immutable fact with provenance. | `target` | `sovereign-record` | 6 | Append-only; identity is a content hash, never a counter, sequence number, or address (ARCH_PRINCIPLES §7.5). |
| **`Endpoint`** | a network surface with a declared exposure. | `target` | `sovereign-mesh` | 6 | Declaring an endpoint declares its exposure class (loopback \| mesh-internal \| private-lan \| token-authenticated). |
| **`Gap`** | a demand for evidence the current corpus cannot meet. | `target` | `sovereign-contracts` | 6 | A Gap is a demand for evidence the current corpus cannot meet — statement, what would answer it, and the trail from claim to demand. |
| **`Recipe`** | how a corpus is made. | `holds` | `corpus-engine` | — | Pure TOML declaring acquire → extract → filter → chunk → embed → index, plus custody. |
| **`Corpus`** | an index plus its custody, produced by a `Recipe`. | `holds` | `corpus-engine` | — | An index plus its sharing policy, produced by a Recipe. |
| **`Claim`** | one assertion extracted from a `Draft`; the unit the gate judges. | `holds` | `sovereign-core` | — | One assertion extracted from a Draft — the unit the gate judges. |
| **`Atom`** | the enrichment ontology's unit (`AtomEnvelope`). | `holds` | `corpus-engine` | — | The closed set of atlas atom kinds (Entity, Event, State, Relation, Claim, Question, Configuration, ArgumentReconstruction, Position), tagged, with deliberately no #[serde(other)] — an unknown atom refuses, never skips. |
| **`Tool`** | one manifest of tool identity, effect and scope (`ToolRegistry`). | `holds` | `sovereign-tools` | — | One manifest of tool identity, effect and scope; a tool exists iff the registry lists it. |
| **`Peer`** | another node in the trust ring. | `holds` | `commonwealth-core` | — | Identity, transport, advertised capabilities. |
| **`NodeCapability`** | what a peer advertises (OICP). | `holds` | `oicp-types` | — | The OICP manifest — what a node advertises. |

**Declared shapes.** 9 of the 31 rows carry a type sketch in the register; the rest are carried by their totality rule alone. A sketch is the TARGET spelling — where the status marker reads `target`, no such type exists yet, and the graph evidence below says so per row.

**`Measurement`** · `target` — one comparable observation.

```rust
pub struct Measurement {
    pub fingerprint: Fingerprint,   // THE KEY, not metadata
    pub metrics:     BTreeMap<MetricName, f64>,
    pub artifact:    ArtifactRef,
}

pub struct Fingerprint {            // every field required
    pub lane:           LaneId,
    pub corpus:         CorpusId,
    pub model:          ModelAttribution,
    pub prompt_version: PromptVersion,
    pub sample_cap:     SampleCap,     // absent entirely from today's baseline
    pub scorer_version: ScorerVersion,
}
```

**`Verdict`** · `target` — the outcome of any check.

```rust
pub enum Verdict {
    Passed,
    Failed(Reason),
    CouldNotJudge(Reason),
    NeverRan(Reason),
}
```

**`Judgement`** · `target` — a verdict with its evidence and calibration.

```rust
pub enum Judgement {
    Judged {
        verdict:  JudgedVerdict,      // Passed | Failed(Reason) | CouldNotJudge(Reason)
        quote:    EvidenceQuote,      // inside the variant — not Option-with-a-comment
        register: Attribution,        // which judge, which build, which prompt
    },
    NeverRan(Reason),                 // carries no quote BY CONSTRUCTION
}
```

**`Origin`** · `target` — where a piece of knowledge came from.

```rust
pub struct Origin {
    pub source:    Source,    // the CLOSED sum — see below
    pub served_by: Server,    // Local | Peer(NodeId, PeerName)
    pub grain:     Grain,     // Leaf | Summary — what this may ground
}

pub enum Source {
    Corpus     { corpus: CorpusId, document: DocumentId, locator: Locator },
    Web        { url: Url, fetched_at: Timestamp },
    Attachment { asset: AssetId, locator: Locator },
    ToolOutput { tool: ToolId, call_hash: ContentHash },
}
```

**`SharingPolicy`** · `target` — what a *corpus* permits the mesh.

```rust
pub enum SharingPolicy {
    Local,                                        // never leaves
    Mesh { queryable: bool, replicable: bool },   // snippets / index bytes
}
```

**`Evidence`** · `target` — the retrieval unit, and the only thing a model may read.

```rust
pub struct Evidence {
    pub content: Text,
    pub origin:  Origin,
    pub custody: Custody,
    pub score:   Relevance,
}
```

**`Answer`** · `partial` — what the user sees.

```rust
pub struct Answer {
    pub text:       Text,
    pub citations:  Vec<Citation>,   // each points into the sealed EvidenceSet
    pub provenance: Provenance,      // a field, not metadata["provenance"]
    pub judgement:  Judgement,
}
```

**`Capabilities`** · `target` — what this install can actually do.

```rust
pub struct Capabilities { /* validated at construction */ }

pub enum Capability<T> {
    Available(T),
    Unavailable(UnavailableReason),
}
```

**`Record`** · `target` — an immutable fact with provenance.

```rust
pub struct Record<T> {
    pub id:         ContentHash,  // identity from essence, never a counter
    pub written_at: Timestamp,
    pub author:     Author,
    pub body:       T,
}
```
<!-- END GENERATED register -->

### 2.1 What the graph says about each of them

The register is a set of declarations. This is the same set checked against the
SCIP graph, which is the half a hand-written document cannot do for itself.

<!-- BEGIN GENERATED graph-evidence -->
<!-- measured_at=1787276979 register_digest=fnv6f60692a8fb896df -->
*Measured 2026-08-21T01:49:39Z by `scripts/nc-pressure.py`, against graph commit `bcabbb694106` — **not** your working tree.*

What the SCIP graph says about each register row, joined by noun name. `defs` is first-party production definitions of that exact name; `kin` is names that end or start with it; `sites` is reference sites. The verdict column is the one that matters — it is the register's declared shape checked against the graph, and a disagreement renders as a disagreement.

| noun | declared | disposition | defs | kin | sites | graph verdict |
|---|---|---|---:|---:|---:|---|
| **`Measurement`** | `target` | converge | 0 | 5 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-eval` will own it |
| **`Baseline`** | `partial` | converge | 0 | 6 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-eval` will own it |
| **`Attribution`** | `target` | converge | 1 | 7 | 16 | **ELSEWHERE** — the one definition is in `kernel-types`, the register declares `sovereign-contracts` |
| **`Verdict`** | `target` | converge the gate family; distinct (rename) the rest | 10 | 38 | 440 | **DUPLICATED** (10) — defined in `sovereign-cli-llm`, `sovereign-eval`, `sovereign-eval`, `sovereign-eval`, `sovereign-mesh`, `sovereign-authoring-harness`, `corpus-engine-archaeology`, `sovereign-core`, `corpus-engine`, `kernel-types` |
| **`Judgement`** | `target` | converge per question: three cores, two renames | 1 | 0 | 96 | **ELSEWHERE** — the one definition is in `kernel-types`, the register declares `sovereign-contracts` |
| **`WireMessage`** | `target` | external-mirror (OpenAI family) + converge (true protocol DTOs) + distinct (same-name different protocols) | 0 | 0 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-wire` will own it |
| **`SurfaceError`** | `partial` | converge | 0 | 0 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-desktop` will own it |
| **`BootstrapMode`** | `partial` | converge | 1 | 0 | 43 | converged — one definition, in `sovereign-desktop` as declared |
| **`Origin`** | `target` | converge | 1 | 2 | 22 | **ELSEWHERE** — the one definition is in `kernel-types`, the register declares `sovereign-contracts` |
| **`Custody`** | `partial` | converge | 1 | 2 | 221 | **ELSEWHERE** — the one definition is in `kernel-types`, the register declares `sovereign-contracts` |
| **`SharingPolicy`** | `target` | converge | 0 | 0 | 0 | **ABSENT** — no definition anywhere; the register declares `corpus-engine` will own it |
| **`Evidence`** | `target` | converge (and `layered` for the contracts ScoredChunk wrapper — see today) | 2 | 17 | 19 | **DUPLICATED** (2) — defined in `sovereign-tools`, `corpus-engine` |
| **`EvidenceSet`** | `target` | converge | 1 | 0 | 8 | **ELSEWHERE** — the one definition is in `corpus-engine`, the register declares `sovereign-contracts` |
| **`Draft`** | `target` | converge (the turn concept); distinct (two squatters rename) | 3 | 11 | 37 | **DUPLICATED** (3) — defined in `sovereign-cli-llm`, `sovereign-core`, `kernel-types` |
| **`Answer`** | `partial` | converge | 1 | 11 | 16 | converged — one definition, in `kernel-types` as declared |
| **`Citation`** | `target` | converge | 4 | 10 | 44 | **DUPLICATED** (4) — defined in `sovereign-server`, `sovereign-meshapp`, `sovereign-eval`, `kernel-types` |
| **`Capabilities`** | `target` | converge | 0 | 7 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-core` will own it |
| **`Step`** | `partial` | converge (the dataflow substrate); distinct (plan-step and build-stage squatters) | 3 | 33 | 67 | **DUPLICATED** (3) — defined in `sovereign-cli-dev`, `sovereign-workflow`, `sovereign-contracts` |
| **`Artifact`** | `partial` | converge (substrate + DR tenancy); distinct (bench output record renames) | 3 | 10 | 57 | **DUPLICATED** (3) — defined in `sovereign-cli-llm`, `sovereign-workflow`, `sovereign-core` |
| **`Intent`** | `partial` | converge | 1 | 8 | 535 | converged — one definition, in `sovereign-contracts` as declared |
| **`Command`** | `partial` | converge | 1 | 1 | 4 | the register's canonical `sovereign/docs/cli-contract.toml` is not a crate path, so the graph cannot be asked where this noun lives |
| **`Record`** | `target` | converge | 0 | 50 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-record` will own it |
| **`Endpoint`** | `target` | converge | 0 | 3 | 0 | **ABSENT** — no definition anywhere; the register declares `sovereign-mesh` will own it |
| **`Gap`** | `target` | converge (the knowledge-gap family); distinct (design/ignorance gaps rename) | 5 | 14 | 47 | **DUPLICATED** (5) — defined in `sovereign-cli-dev`, `sovereign-cli-dev`, `sovereign-core`, `corpus-engine`, `sovereign-contracts` |
| **`Recipe`** | `holds` | distinct | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`Corpus`** | `holds` | converge | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`Claim`** | `holds` | converge (the spec-intel mirror pair); the atlas Claim atom is a distinct, correctly-named concept | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`Atom`** | `holds` | converge | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`Tool`** | `holds` | converge | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`Peer`** | `holds` | converge | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |
| **`NodeCapability`** | `holds` | converge | — | — | — | not measured — `in_program = false`, so the instrument does not visit it |

**8 absent · 5 elsewhere · 7 duplicated · 7 not measured.** Excess definitions (the judged number, target 0): 23. Nouns with a canonical: 16/24.
<!-- END GENERATED graph-evidence -->

## 3. The verbs

Eight for the product, two for the system's view of itself. That is the
whole surface; everything else is a helper. *(v1 declared six and the two
absences were where nouns went unregistered: `enrich` was missing and so
was the whole Atom family; `route` was missing and `Intent`'s per-intent
fan-out went undispositioned. A verb absent from this table is where the
next undeclared noun grows.)*

| Verb | Signature | Guarantee |
|---|---|---|
| **acquire** | `Source → Vec<Record>` | every record carries author + timestamp + content hash |
| **index** | `Vec<Record> × Recipe → Corpus` | sharing policy is stamped at index time, from the recipe |
| **enrich** | `Corpus → Atlas` | every atom carries provenance to its source chunks; an unknown atom kind refuses |
| **route** | `Question → Intent` | one classifier, one calibration; the intent table is the only per-intent decider |
| **retrieve** | `Question × Capabilities → EvidenceSet` | **the only producer of `Evidence`**; seals the scope |
| **compose** | `EvidenceSet × Intent → Draft` | the model sees the seal and nothing else |
| **verify** | `Draft × EvidenceSet → Vec<Judgement>` | support judged against the set compose read; may search the sealed scope to check a claim, never widen it |
| **release** | `Draft × Vec<Judgement> → Answer` | no `Answer` without a `Judgement`; abstention is an `Answer`; one named fold policy |
| **measure** | `Run → Measurement` | fingerprint stamped at capture, from the build |
| **converge** | `Measurement × Baseline → Verdict` | refuses across fingerprints rather than guessing |

### 3.1 The pipeline

```
     Source
        │  acquire
        ▼
   Vec<Record> ────── Recipe ──────┐
        │  index                   │  custody stamped here
        ▼                          ▼
     Corpus  ═══════════════════════
        │
        │  retrieve  (Question × Capabilities)        ← THE ONLY DOOR
        ▼
   EvidenceSet ──────────────────────── sealed ───────┐
        │                                             │
        │  compose (Intent)                           │
        ▼                                             │
      Draft                                           │
        │                                             │
        │  verify ─────────────────────────────────────
        ▼
  Vec<Judgement>
        │  release
        ▼
     Answer   (text + citations + provenance + judgement)
```

Two properties make this unusual, and both are structural rather than
procedural:

1. **`retrieve` is the only door.** `Evidence` has no other constructor, so
   no code path from a corpus to a prompt can skip it, and none can produce
   content without an origin.
2. **`verify` receives the same `EvidenceSet` value `compose` received,
   and the set carries two seals** — the chunk set and the corpus scope.
   Support is judged against the chunk set the model actually saw;
   verification may search the sealed scope to check a claim, and anything
   it finds is marked verification-found in the `Judgement`. A gate that
   judges support against different evidence than the model saw is checking
   the wrong thing — and today's gate does exactly that (a filtered,
   reordered projection under two env vars, plus live re-search against a
   corpus-id list reconstructed from the chunks). Passing one value to both,
   with the asymmetric rule, makes the mistake unrepresentable without
   banning the re-search verification needs.

`ComplexTask` rides the same pipeline, plus an idempotency ledger (tool
steps run exactly once across crash and replay) and a delegate firewall (the
worker sees raw output, the orchestrator only a typed contract). *(`holds`)*

### 3.2 The substrate

`compose` and the corpus pipeline are both **dataflow over typed artifacts**,
and there is one engine for that: `Step` · `Artifact` · `Runner`, with
content-addressed caching and a step registry. Five tenants, one engine.
*(`partial` — the engine exists and is wired in as a product feature rather
than as substrate; see the `Step` row in the register.)*

---

## 4. Custody — the trust model as types

The mesh's promise is *chunks travel, corpora don't*. It is enforced on
**two axes at two altitudes** — corpus-level policy decided before any
`Evidence` exists, and chunk-level custody carried by the evidence itself:

| Question | Answered by | Enforced at |
|---|---|---|
| may a peer search this corpus and receive snippets? | `SharingPolicy::Mesh { queryable }` | capability advertising |
| may the index bytes replicate? | `SharingPolicy::Mesh { replicable }` | snapshot replication + index transfer |
| may this corpus leave the machine at all? | `SharingPolicy::Local` | both, structurally |
| may *this piece of evidence* enter a peer-bound reply? | `Evidence.custody` | response construction |

**The crossing.** Evidence arriving from a peer has
`Origin.served_by = Server::Peer(node_id, name)` — a required field of a
required struct, not a nullable map entry. The wire type is shared by both
projects, so attribution cannot be dropped in translation, and a
locally-served hit is `Server::Local` rather than an absent key.

**The refusal.** A corpus with `SharingPolicy::Local` yields evidence whose
custody forbids sharing. The response builder accepts only shareable
evidence when constructing a peer-bound reply. There is no runtime check to
forget, because the illegal construction does not typecheck.

---

## 5. Deployment shapes

One binary surface, several profiles. A profile is **declared and
validated**, so "which capabilities are wired" is a value you can print,
test and diff — not an emergent property of which builder methods a call
site happened to chain.

| Profile | Capabilities | Surfaces |
|---|---|---|
| `assistant-local` | inference, corpora, tools | CLI, desktop |
| `assistant-mesh` | + peers, federated retrieval, tensor split | + mesh internal |
| `daemon-headless` | inference, corpora, no UI | HTTP `:9741`, MCP |
| `server-multitenant` | + tenancy, approvals | HTTP `:8080` |
| `bench` | deterministic inference, frozen corpora | measurement only |

**The test that could not be written before:** the configuration matrix is
enumerable, so a suite runs the top-N legal profiles and asserts each
behaves as declared.

---

## 6. Layer map

The refactor reduces **concepts**, not necessarily crates. Stating that
plainly because it is the likeliest misreading: 59 crates is not the
problem, and merging crates is not the goal.

<!-- BEGIN GENERATED layer-map -->
Read from [`ARCH_LAYERS.toml`](./ARCH_LAYERS.toml) — the same file `cargo xtask layer-gate` enforces against Cargo-declared edges, so the map below and the map that fails the build are one map (§10.6).

| tier | layer | crates (as declared, `*` is a pattern) |
|---:|---|---|
| 0 | **contract** | `oicp-types` · `kernel-types` · `sovereign-contracts` · `oicp-client` · `arch-layers` · `sovereign-time` |
| 1 | **knowledge** | `corpus-engine` · `corpus-engine-*` |
| 2 | **mesh-foundation** | `commonwealth-core` · `commonwealth-state` · `commonwealth-transport` · `commonwealth-discovery` · `commonwealth-inference` · `commonwealth-knowledge` · `commonwealth-app` |
| 3 | **runtime** | `sovereign-core` · `sovereign-inference` · `sovereign-store` · `sovereign-workflow` · `sovereign-tools-base` · `sovereign-recipe-author` · `sovereign-compute` · `sovereign-gliner` |
| 4 | **capabilities** | `sovereign-tools` · `sovereign-enrichment-catalog` · `sovereign-work-atlas` · `sovereign-workflow-host` · `sovereign-atos` · `sovereign-eval` · `sovereign-meshapp` · `sovereign-authoring-harness` · `commonwealth-agent-tools` |
| 5 | **mesh-api** | `commonwealth-api` · `commonwealth-test-harness` · `commonwealth-tdd` · `sovereign-mesh` |
| 6 | **hosts** | `sovereign-cli*` · `sovereign-server` · `sovereign-desktop` · `sovereign-pipeline` · `sovereign-agent-bench` · `sovereign-studio` · `commonwealth-daemon` · `oicp-conformance` · `xtask` |

**Back of house** — outside the ordered stack, not on top of it; may observe every layer, and nothing may depend on it: `sovereign-eval` · `sovereign-cli-dev` · `sovereign-agent-bench` · `sovereign-atos` · `corpus-engine-atos` · `corpus-engine-archaeology` · `xtask`.

**4 grandfathered violation(s)** ride `[[exception]]` entries — each one says the boundary is drawn in the wrong place and the crate split has not been paid for, not that the edge is fine:

- `sovereign-cli-llm` → `sovereign-eval` — bench_cmd (51 files, ~31k lines) shares this bin-only crate with chat/corpus/mesh, so the shipped binary links the instrument. Priced 2026-08-20 and declined: extracting it needs a [lib] over ~130k lines plus pub churn through enrich_cmd/eval_cmd/chat_cmd. CONTAINED meanwhile by the module rule bench_cmd_is_the_only_module_naming_the_eval_harness (src/main.rs) — one module names it, and a test fails if that changes. The product verb that used to cross (recipe_cmd, via the sovereign_eval::authoring_harness alias) was repointed at sovereign-authoring-harness directly and no longer does.
- `commonwealth-api` → `sovereign-core` — the mesh API embeds the agent runtime for in-process serving (frontdoor); goal state is the OICP seam
- `commonwealth-api` → `sovereign-tools` — tool registry assembly for the embedded runtime; goal state is the OICP seam
- `commonwealth-api` → `sovereign-atos` — feature-gated (atos) ATOS surface on the mesh API; opt-in experiment, off in default builds
<!-- END GENERATED layer-map -->

Direction is one-way, enforced by `cargo xtask layer-gate` against
`quality/ARCH_LAYERS.toml` (Cargo-declared edges) plus the SCIP arch report
(observed edges). *(The gate `holds`; the tier assignment is the target.)*

**The rule that keeps it honest:** a noun is defined in the lowest tier that
needs it and re-exported upward. A surface never declares its own copy of a
Tier-1 noun. That single rule is what the concept ratchet enforces, and it
is why the ratchet is the mechanism rather than a report.

### 6.1 How wide the domain boundaries actually are

A layer map says which direction an edge may run. It does not say how much
crosses. This does — measured, not asserted.

<!-- BEGIN GENERATED boundary -->
<!-- measured_at=1787276979 register_digest=none -->
*Measured 2026-08-21T01:49:39Z by `scripts/nc-boundary.py`, against graph commit `bcabbb694106` — **not** your working tree.*

A domain boundary is only a boundary if a small, named set of types crosses it. `width` is distinct types referenced across the edge; `refs` is how often. A `flag` names an edge that should not exist at all.

| from | to | refs | width | flag |
|---|---|---:|---:|---|
| `sovereign` | `corpus-engine` | 4744 | 344 | — |
| `back-of-house` | `sovereign` | 556 | 117 | — |
| `sovereign` | `commonwealth` | 557 | 91 | — |
| `sovereign` | `back-of-house` | 447 | 79 | **BACKSTAGE** |
| `sovereign` | `oicp` | 2656 | 54 | — |
| `sovereign` | `studio` | 337 | 44 | **BACKFLOW** |
| `commonwealth` | `oicp` | 419 | 40 | — |
| `back-of-house` | `corpus-engine` | 327 | 38 | — |
| `studio` | `sovereign` | 1043 | 34 | — |
| `commonwealth` | `corpus-engine` | 206 | 31 | — |
| `studio` | `oicp` | 285 | 18 | — |
| `commonwealth` | `sovereign` | 38 | 11 | **BACKFLOW** |
| `corpus-engine` | `kernel` | 74 | 11 | — |
| `sovereign` | `kernel` | 300 | 7 | — |
| `commonwealth` | `back-of-house` | 27 | 6 | **BACKSTAGE** |
| `back-of-house` | `oicp` | 28 | 4 | — |
| `oicp` | `sovereign` | 39 | 3 | **BACKFLOW** |
| `back-of-house` | `commonwealth` | 26 | 2 | — |
| `commonwealth` | `kernel` | 573 | 1 | — |

- **core boundary width** (the three systems): 477
- **types on edges that should not exist**: 143
- **shared kernel** — types all three systems speak: 23
- kernel owned by: `corpus-engine` 23

**Every kernel type is owned by one domain.** That is not a contract, it is a dependency on an implementation.
<!-- END GENERATED boundary -->

---


## 7. Structural vs. gated

§7 — *make it structural, not remembered.* The balance after the refactor:

| Invariant | Today | Target |
|---|---|---|
| Every answer is grounded in evidence | **type** (`kernel_types::Answer`, rung 11) — the runtime turn path has not migrated onto it | **type** — no `Answer` without `Judgement` |
| Sharing policy is respected | ~149 raw-bool sites, ~5:1 plumbing to guards | **type** — one `SharingPolicy` + one resolution decider; the true guards remain, the plumbing collapses |
| Chunk custody is respected | **type** (`PeerAnswer`, rung 11) at the mesh boundary, which had no check at all before it; still a metadata string key on the retrieval path | **type** — non-shareable evidence cannot be constructed into a peer-bound reply |
| Provenance is present | **type** (`Answer.provenance: Attribution`, rung 11) — a required argument of every release door; the 8 `metadata["provenance"]` writer sites are unmigrated | **type** — a required field |
| The model never originates a number | runtime audit | audit **plus** the value's `Origin` |
| A capability is wired | `if let Some(..)` | **type** — `Capability<T>` |
| Two measurements are comparable | operator memory | **key** — fingerprint |
| A command exists | 3 reconciliation harnesses | **generated** from the contract |
| An endpoint is guarded | mounted by hand, 6 of 8 | **registry** attaches the guard |
| Docs match code | ~16,600 lines of drift detection | **generated** from `CONCEPTS.toml` |

**Read the middle column exactly as written.** Rung 11 minted the types and
proved by compile-fail that the illegal constructions have no spelling
(`kernel-types/tests/answer_reds.rs`, six fixtures, each watched failing before
its `.stderr` was recorded). It did NOT migrate the live turn path:
`grounding/mod.rs` still assembles `ReleasedCitation` by hand and
`streaming.rs` still holds tokens procedurally. A row reading "type" where the
product does not yet use the type is a real change — the construction is now
impossible for anyone who reaches for the noun — and it is not the same as a
migrated path. Saying otherwise here is exactly the well-formed, exit-0,
wrong result ARCH §18 exists to catch.

Gates remain for what types genuinely cannot hold: answer quality, judge
calibration, retrieval recall, honesty under adversarial questioning. Those
are measured, not asserted — and after phase 1, measured with an instrument
that knows when it is being misread.

---

## 8. How this stays true

Four regions of this document are **generated**, and the generator is a gate:

```
cargo run -p xtask -- target-arch                 # check — four verdicts, never two
cargo run -p xtask -- target-arch --update-doc    # re-render the DECLARED blocks
cargo run -p xtask -- target-arch --measure       # also re-run the instruments
```

| block | source | kind |
|---|---|---|
| §2 register | `quality/CONCEPTS.toml` | declared — re-rendered and diffed on every check |
| §6 layer map | `quality/ARCH_LAYERS.toml` (the file `layer-gate` enforces) | declared — same |
| §2.1 graph evidence | `scripts/nc-pressure.py` → `svrn code converge noun` | measured — carries a stamp |
| §6.1 boundary | `scripts/nc-boundary.py` | measured — carries a stamp |

A declared block that disagrees with its source is STALE and fails. A measured
block is not re-derived on every run — it describes the last indexed commit,
not your working tree, and a gate that flaps with the indexer gets switched
off inside a week. Instead each measured block carries the time it was taken,
the graph commit it was taken against, and the digest of the register it was
joined to; the check reads that stamp. Too old, or joined to a register that
has since changed, is COULD-NOT-JUDGE. A block that is missing entirely is
NEVER-RAN. Neither is a pass.

**What is still hand-maintained, named rather than left implied:** §1 (the
thesis), §3 (the verbs), §4 (custody as a trust model), §5 (deployment
shapes), §7 (structural vs. gated). Those are arguments and intentions, not
structure, and no register row backs them — which means they carry exactly the
decay this section used to warn about, and the warning still applies to them:

> The 2026-08-16 draft was verified against the code on 2026-08-17 — one day
> later — and six of its register rows were materially stale or wrong. The
> counts all reproduced; the *prose qualifiers* had decayed.

That one-day half-life is why the register's evidence fields carry a date and
a `measure` method rather than bare numbers: prose carries methods, tools
carry numbers. §9's glossary is gone — it was a third hand-kept spelling of
the register's `gloss` field, and the table in §2 renders that field directly.

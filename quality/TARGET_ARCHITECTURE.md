# Target architecture — the nouns and verbs

**What this is.** The destination of the
[noun-convergence program](./NOUN_CONVERGENCE.md): the architecture after
the refactor, organised around what the system is made of and what may be
done with it, rather than around where code lives.

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

Twenty. Each has exactly one canonical definition, one owning crate, and a
**totality rule** — the thing that must always be true, encoded so it cannot
be forgotten.

### 2.1 Knowledge

**`Origin`** · `target` — where a piece of knowledge came from.

```rust
pub struct Origin {
    pub corpus:    CorpusId,
    pub document:  DocumentId,
    pub locator:   Locator,   // page / section / offset — typed, not a string
    pub served_by: Server,    // Local | Peer(NodeId, PeerName)
}
```

*Totality:* every field non-optional. No constructor yields an `Origin`
without a corpus and a document.

**`Custody`** · `target` — what may be done with a piece of knowledge.

```rust
pub struct Custody {
    pub queryable_by_peers: bool,  // may a peer search this and get snippets
    pub replicable:         bool,  // may the index bytes move
    pub scope:              Scope, // Local | Mesh
}
```

*Totality:* carried **by the data it governs**, not looked up from a
registry at the point of use. A value that may not leave cannot be
constructed into a shareable response — the compiler refuses, not the
reviewer.

**`Evidence`** · `target` — the retrieval unit, and the only thing a model
may read.

```rust
pub struct Evidence {
    pub content: Text,
    pub origin:  Origin,
    pub custody: Custody,
    pub score:   Relevance,
}
```

*Totality:* constructible **only** by `retrieve`. No public constructor, no
`Default`, no path from raw text to `Evidence` that skips origin and
custody.

**`EvidenceSet`** · `partial` — a sealed body of evidence for one turn.

*Totality:* the seal is fixed at retrieval. Verification may widen *search*
to check a claim; it may never widen the *seal*. Composition reads from the
set and cannot reach past it.

**`Corpus`** · `holds` — an index plus its custody, produced by a `Recipe`.

**`Recipe`** · `holds` — how a corpus is made. Pure TOML declaring
acquire → extract → filter → chunk → embed → index, plus custody.

> **This is the reference implementation of the whole architecture.** It is
> the one place where a policy decision is already data rather than code,
> proven across 26 catalogued recipes with schema generation and regression
> fixtures. Every `target` noun in this document is an application of the
> move `Recipe` already made — which is why the program is a generalisation
> of our own best abstraction, not the import of a foreign one.

**`Record`** · `target` — an immutable fact with provenance.

```rust
pub struct Record<T> {
    pub id:         ContentHash,  // identity from essence, never a counter
    pub written_at: Timestamp,
    pub author:     Author,
    pub body:       T,
}
```

*Totality:* append-only; identity is a content hash (§7.5). Mutable views
are projections — derived, rebuildable.

### 2.2 The turn

**`Question`** · `holds` — the user's message plus conversation context.

**`Intent`** · `partial` — what kind of ask it is. Thirteen variants, plus a
data table.

*Totality:* every per-intent attribute is a column in one table. Adding an
intent is one row; a missing attribute is a compile error, not a fallback
arm.

**`Draft`** · `partial` — generated text, not yet released.

*Totality:* a `Draft` cannot be returned to a surface. It becomes an
`Answer` only by passing through `verify` and `release`, which makes "forgot
to run the gate" unrepresentable.

**`Claim`** · `holds` — one assertion extracted from a `Draft`; the unit the
gate judges.

**`Verdict`** · `target` — the outcome of any check.

```rust
pub enum Verdict {
    Passed,
    Failed(Reason),
    CouldNotJudge(Reason),
    NeverRan(Reason),
}
```

*Totality:* four states, one definition, workspace-wide. §18.2 becomes a
type instead of a rule people remember. `NeverRan` is distinct from
`CouldNotJudge` — a gate that did not execute is not a gate that abstained.

**`Judgement`** · `target` — a verdict with its evidence and calibration.

```rust
pub struct Judgement {
    pub verdict:  Verdict,
    pub quote:    Option<EvidenceQuote>,  // None only for NeverRan
    pub register: RegisterFingerprint,    // which judge, which build, which prompt
}
```

*Totality:* one implementation of *does this text follow from this
evidence?*, one calibration gate. Lanes bind as tenants; none owns a private
copy (§10.6).

**`Answer`** · `partial` — what the user sees.

```rust
pub struct Answer {
    pub text:       Text,
    pub citations:  Vec<Citation>,   // each points into the sealed EvidenceSet
    pub provenance: Provenance,      // a field, not metadata["provenance"]
    pub judgement:  Judgement,
}
```

*Totality:* cannot exist without a `Judgement`. **An honest abstention is an
`Answer` too** — `Verdict::Failed` with a reason — which is why abstention
is a first-class outcome rather than an error path.

### 2.3 Deployment

**`Capabilities`** · `target` — what this install can actually do.

```rust
pub struct Capabilities { /* validated at construction */ }

pub enum Capability<T> {
    Available(T),
    Unavailable(UnavailableReason),
}
```

*Totality:* a declared, validated profile. Consumers receive a **narrowed
view** carrying only what they need. `Unavailable` is a value the caller
must handle; it never silently becomes an empty result.

**`Endpoint`** · `target` — a network surface with a declared exposure.

*Totality:* declaring an endpoint declares its exposure class
(loopback / mesh-internal / authenticated). The guard is attached by the
registry, not mounted by hand.

**`Command`** · `partial` — one CLI promise.

*Totality:* `cli-contract.toml` is **generative** — parser, dispatch, help
and reference docs are produced from it. A command not in the contract does
not exist; a contract row nothing implements does not compile.

### 2.4 The mesh

**`Peer`** · `holds` — another node in the trust ring. Identity, transport,
advertised capabilities; founded by a join key plus an Ed25519
proof-of-possession.

**`NodeCapability`** · `holds` — what a peer advertises (OICP). A clean
contract with a standalone conformance tester.

### 2.5 Self-measurement

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

*Totality:* the fingerprint is the *key*. Two measurements with different
fingerprints are not comparable, and the gate does not attempt it — it
reports *no baseline for this fingerprint → first run → pass*.

**`Baseline`** · `partial` — the committed prior for one fingerprint.

*Totality:* stored under its fingerprint, so a changed condition changes the
key and a stale baseline cannot silently false-fire.

---

## 3. The verbs

Six for the product, two for the system's view of itself. That is the whole
surface; everything else is a helper.

| Verb | Signature | Guarantee |
|---|---|---|
| **acquire** | `Source → Vec<Record>` | every record carries author + timestamp + content hash |
| **index** | `Vec<Record> × Recipe → Corpus` | custody is stamped at index time, from the recipe |
| **retrieve** | `Question × Capabilities → EvidenceSet` | **the only producer of `Evidence`**; seals the scope |
| **compose** | `EvidenceSet × Intent → Draft` | the model sees the seal and nothing else |
| **verify** | `Draft × EvidenceSet → Vec<Judgement>` | judges against the *same* seal composition used |
| **release** | `Draft × Vec<Judgement> → Answer` | no `Answer` without a `Judgement`; abstention is an `Answer` |
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
2. **`verify` reads the same seal `compose` read** — the identical
   `EvidenceSet` value, not a fresh search. A gate that verifies against
   different evidence than the model saw is checking the wrong thing;
   passing one value to both makes that mistake unrepresentable.

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

The mesh's promise is *chunks travel, corpora don't*. After the refactor
that is a property of `Evidence`, checked once at construction.

| Question | Answered by | Enforced at |
|---|---|---|
| may a peer search this and receive snippets? | `Custody.queryable_by_peers` | capability advertising |
| may the index bytes replicate? | `Custody.replicable` | snapshot replication + index transfer |
| may this leave the machine at all? | `Custody.scope == Local` | both, structurally |

**The crossing.** Evidence arriving from a peer has
`Origin.served_by = Server::Peer(node_id, name)` — a required field of a
required struct, not a nullable map entry. The wire type is shared by both
projects, so attribution cannot be dropped in translation, and a
locally-served hit is `Server::Local` rather than an absent key.

**The refusal.** A corpus with `scope = Local` yields evidence whose custody
forbids sharing. The response builder accepts only shareable evidence when
constructing a peer-bound reply. There is no runtime check to forget,
because the illegal construction does not typecheck.

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

```
Tier 5  surfaces      sovereign-cli · sovereign-desktop · sovereign-server · sovereign-mobile
Tier 4  composition   sovereign-core (the turn) · sovereign-mesh
Tier 3  capability    corpus-engine · sovereign-inference · sovereign-tools · sovereign-eval
Tier 2  substrate     sovereign-workflow (Step·Artifact·Runner) · sovereign-record
Tier 1  contracts     sovereign-contracts (the nouns) · sovereign-wire
Tier 0  leaves        oicp-types · sovereign-time · corpus-engine-yield
```

Direction is one-way, enforced by `cargo xtask layer-gate` against
`quality/ARCH_LAYERS.toml` (Cargo-declared edges) plus the SCIP arch report
(observed edges). *(The gate `holds`; the tier assignment is the target.)*

**The rule that keeps it honest:** a noun is defined in the lowest tier that
needs it and re-exported upward. A surface never declares its own copy of a
Tier-1 noun. That single rule is what the concept ratchet enforces, and it
is why the ratchet is the mechanism rather than a report.

---

## 7. Structural vs. gated

§7 — *make it structural, not remembered.* The balance after the refactor:

| Invariant | Today | Target |
|---|---|---|
| Every answer is grounded in evidence | runtime gate | **type** — no `Answer` without `Judgement` |
| Custody is respected | 147 call sites | **type** — non-shareable evidence cannot be constructed into a shared reply |
| Provenance is present | per-path convention | **type** — a required field |
| The model never originates a number | runtime audit | audit **plus** the value's `Origin` |
| A capability is wired | `if let Some(..)` | **type** — `Capability<T>` |
| Two measurements are comparable | operator memory | **key** — fingerprint |
| A command exists | 3 reconciliation harnesses | **generated** from the contract |
| An endpoint is guarded | mounted by hand, 6 of 8 | **registry** attaches the guard |
| Docs match code | ~16,600 lines of drift detection | **generated** from `CONCEPTS.toml` |

Gates remain for what types genuinely cannot hold: answer quality, judge
calibration, retrieval recall, honesty under adversarial questioning. Those
are measured, not asserted — and after phase 1, measured with an instrument
that knows when it is being misread.

---

## 8. How this stays true

It is **generated** from [`CONCEPTS.toml`](./CONCEPTS.toml) plus the SCIP
graph. The noun list, status markers, totality rules and layer map are
derived, not typed by hand.

That is the program applied to itself: a hand-maintained description needs a
machine to check it, and that machine is ~16,600 lines that itself drifts
and needs its own posture command. A generated description needs none.

Until generation lands (phase 5), this file is hand-maintained and therefore
subject to exactly the disease it describes. **Treat its `holds` markers as
claims to verify, not facts to cite** — §11.1.

---

## 9. Glossary

| Term | Meaning |
|---|---|
| **Evidence** | content + origin + custody + relevance. Produced only by `retrieve`. The only thing a model reads. |
| **EvidenceSet** | a sealed body of evidence for one turn. Verification may widen search, never the seal. |
| **Origin** | corpus + document + locator + which machine served it. Every field required. |
| **Custody** | queryable-by-peers, replicable, scope. Carried by the data, not looked up. |
| **Seal** | the corpus scope a turn is bound to, fixed at retrieval. |
| **Verdict** | passed / failed / could-not-judge / never-ran. Four states, one definition. |
| **Judgement** | a verdict plus its evidence quote and the fingerprint of the judge that produced it. |
| **Answer** | text + citations + provenance + judgement. An honest abstention is an `Answer`. |
| **Capability&lt;T&gt;** | `Available(T)` or `Unavailable(reason)`. Absence is a value. |
| **Measurement** | metrics plus the fingerprint of the conditions that produced them. |
| **Fingerprint** | lane + corpus + model + prompt version + sample cap + scorer version. The comparability key. |
| **Record** | an immutable fact with content-hash identity. Views are projections. |
| **Recipe** | the TOML declaring how a corpus is acquired, chunked, indexed and shared. |
| **Step / Artifact / Runner** | the one dataflow substrate — typed steps over cached artifacts. |
| **Profile** | a declared, validated set of capabilities for one deployment. |
| **Intent** | the router's verdict on a message, plus its row in the intent table. |

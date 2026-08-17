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

Twenty-seven — twenty-one from the first draft, six added by the
2026-08-17 verification pass (`Attribution`, `SharingPolicy`, `Gap`,
`Citation`, `Atom`, `Tool`), which also split one (`Custody`) and
re-classified two (`EvidenceSet`, `Question`). Each has exactly one
canonical definition, one owning crate, and a **totality rule** — the thing
that must always be true, encoded so it cannot be forgotten.

### 2.1 Knowledge

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

*Totality:* every field non-optional, and the source is a **closed sum**.
Without the sum, "retrieve is the only door" is false on day one — web
search, attached documents and tool output already reach the model, and an
`Origin` that can only name a corpus would force those paths to stay
untyped. Without `grain`, what a chunk may *ground* stays a string compare
(today: `metadata["source"] == "raptor"` decides Summary-vs-Leaf inside the
gate). `NodeId` comes from a Tier-0/1 home — contracts cannot import the
mesh crate.

**`Custody`** · `partial` — where a piece of content stands, carried by the
chunk. *(v1 fused two concepts under this name; the 2026-08-17 verification
split them. The typed chunk-level class already exists —
`sovereign-contracts/src/types/custody.rs` — and already rides chunks; v1's
"never carried by the data" was false.)*

*Totality:* a required, typed field of `Evidence`, stamped at retrieval
from the corpus's current policy. Promotion is the work: from a string
under `CUSTODY_META_KEY` to a field the compiler sees. Stamped-at-retrieval
means a policy change applies to future seals, not in-flight values —
revocation is a property of the next `retrieve`, by design. A value whose
custody forbids sharing cannot be constructed into a peer-bound response —
the compiler refuses, not the reviewer.

**`SharingPolicy`** · `target` — what a *corpus* permits the mesh.

```rust
pub enum SharingPolicy {
    Local,                                        // never leaves
    Mesh { queryable: bool, replicable: bool },   // snippets / index bytes
}
```

*Totality:* one policy type with one resolution decider, declared by the
`Recipe`, stamped into the index meta, surfaced by the registry. This noun
**stays corpus-level on purpose**: advertising, replication and handoff
decide before any `Evidence` exists, so a carried-by-the-chunk cure aims at
the wrong axis (measured 2026-08-17: ~5:1 of the ~149 consultation sites
are plumbing that collapses into this type; the ~20 true guards remain
because they should). Illegal states unrepresentable: `Local` cannot also
be queryable — today's two loose bools admit that state, and one of them is
`Option<bool>` resolved by an unnamed `unwrap_or` rule at index open.

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

**`EvidenceSet`** · `target` — a sealed body of evidence for one turn.
*(Re-classified from `partial` 2026-08-17: verification found no chunk seal
exists — the gate receives a filtered, reordered projection under two env
vars, and its re-search "seal" is a list of corpus ids, reconstructed from
the chunks themselves when not passed.)*

*Totality:* **two seals, both carried** — the *chunk set* composition read,
and the *corpus scope* retrieval was bound to. Support may come only from
the set; verification may search the scope — to refute or corroborate — and
any verification-found evidence is marked as such in the `Judgement`, never
laundered into the composition's set. `verify()` receives the `EvidenceSet`
value and may not widen the scope. This encodes the asymmetry the gate
actually needs, rather than banning the re-search it depends on.

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
are projections — derived, rebuildable. Tenant zero is the measured pair:
corpus-engine's governance oplog and its enrichment-reconciliation oplog —
the same append-only-JSONL + derived-fold shape, invented twice in sibling
directories, one of them commenting that it "mirrors" the other's
conventions, sharing no type.

**`Gap`** · `target` — a demand for evidence the current corpus cannot
meet.

*Totality:* statement, what would answer it, and the trail from claim to
demand. One noun, three producers, zero private copies — the atlas detects
gaps, the epistemic ledger records them, and the deep-research loop
consumes them as its round-driving input. *(Four shapes of this concept
exist today across three subsystems; it is the central noun of the
deep-research product loop and the v1 register omitted it.)*

**`Atom`** · `holds` — the enrichment ontology's unit (`AtomEnvelope`).

*Totality:* a closed, tagged set of atom kinds with deliberately no
`#[serde(other)]` — an unknown atom refuses, never skips. Listed for
completeness: the atlas is a major noun family the v1 draft omitted even
while citing its fan-out as evidence. (Corrective: variant matching on a
closed enum is what enums are *for* — that fan-out measures exposure, not
disease.)

### 2.2 The turn

**`Question`** · `partial` — the user's message plus conversation context.
*(Downgraded from `holds` 2026-08-17: none of the three types named
`Question` in the census is this concept — they are an atlas atom and two
eval-bank items. The `holds` claim was never verified; §11.1 applies to
this file too. Locate the real canonical or mint it with `Answer`.)*

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
*(Canonical home is Tier-1 `sovereign-contracts` — `Answer` embeds it, and
the tier rule in §6 forbids contracts depending upward on eval. v1's
canonical named a module that does not exist.)*

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

*Totality:* a `Judgement` is **mintable only by a calibrated judge** —
private constructor, the `CalibrationReceipt` capability-token pattern the
gym judge already ships. Same move as "Evidence only by retrieve". One core
per *question*, not one verifier: the 2026-08-17 characterization found the
judge surface asks three questions — **entailment** (`(claim, evidence) →
P(violation)`, one τ), **criterion** (`(subject, predicate) →
Yes/No/CouldNotJudge`, one calibration-bank format), and **rate
aggregation** (one Wilson-CI/paired module) — plus two things that are not
judges at all (the code-quality rubric and the voice telemetry), which get
renamed, not merged. Lanes bind as tenants; none owns a private copy
(§10.6).

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
is a first-class outcome rather than an error path. `release()` folds
`Vec<Judgement>` into the Answer's judgement by **one named policy**
(worst-verdict-wins unless the policy says otherwise) — that fold is a
decider, and a decider gets one name and one implementation (§10.6), not an
ad-hoc reduce at each call site.

**`Citation`** · `target` — a pointer into the sealed `EvidenceSet`.

*Totality:* a `Citation` cannot cite what the seal does not contain. The
audited invariant one surface already states in prose — *the quoted text
appears in the cited chunk* — becomes construction, not audit. (Three types
carry this name today, none canonical; the runtime's `ReleasedCitation` is
a fourth shape under a different name — the synonym class the census is
structurally blind to.)

**`Attribution`** · `target` — which engine computed this text.

*Totality:* **one** answer to model identity, build, quantization, serving
host. `Fingerprint.model` *is* this type; `Judgement.register` names it;
`Answer.provenance` carries it; the OICP manifest advertises it. Four
shapes describe this fact today — a byte-duplicated `ModelInfo` pair, an
unrelated `/v1/models` mirror under the same name, a private capability
`Provenance` enum, and a pinned judge model living in a doc comment as the
comparability guarantee. The fleet's worst attribution incident (the
fast-slot alias hijack, ARCH §10.6's first exhibit) is this noun's absence,
measured.

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
not exist; a contract row nothing implements does not compile. The route
there runs through a **contract-checked waypoint** (generated help and
dispatch asserted against the hand-written code) before contract-generated
— and through adoption of the declarative-parse seam that already exists
(`ArgSpec`, whose own module doc counts the bespoke loops remaining).

**`Tool`** · `holds` — one manifest of tool identity, effect and scope
(`ToolRegistry`). A tool exists iff the registry lists it; tool *output*
that reaches a model enters the evidence system as `Origin::ToolOutput`.

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
reports **`NoBaseline(fingerprint)`, a distinct, posture-visible verdict,
never a silent pass**. A fingerprint change disarms every gate at once; the
re-arm is an explicit baseline re-mint (RUNBOOK §6), not a green tick
nobody earned. *(v1 said "first run → pass" — a shape that lets any
refactor of the bench green-light itself.)*

**`Baseline`** · `partial` — the committed prior for one fingerprint.

*Totality:* stored under its fingerprint, so a changed condition changes the
key and a stale baseline cannot silently false-fire.

---

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
| Sharing policy is respected | ~149 raw-bool sites, ~5:1 plumbing to guards | **type** — one `SharingPolicy` + one resolution decider; the true guards remain, the plumbing collapses |
| Chunk custody is respected | a metadata string key | **type** — non-shareable evidence cannot be constructed into a peer-bound reply |
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

That is not a hypothetical caution. The 2026-08-16 draft was verified
against the code on 2026-08-17 — one day later — and six of its register
rows were materially stale or wrong (`Custody`'s carried-claim, the
`EvidenceSet` seal, `BootstrapMode`'s discarded-payload, the desktop error
counts, the "396 commands", the `Question` holds-marker). The counts all
reproduced; the *prose qualifiers* had decayed. That one-day half-life is
the strongest argument this program has for its own generation clause, and
it is why the register's evidence fields now carry a **date and a
`measure` method** rather than bare numbers: prose carries methods, tools
carry numbers.

---

## 9. Glossary

| Term | Meaning |
|---|---|
| **Evidence** | content + origin + custody + relevance. Produced only by `retrieve`. The only thing a model reads. |
| **EvidenceSet** | two seals for one turn: the chunk set compose read, the corpus scope retrieval was bound to. Support from the set only; verification may search the scope, never widen it. |
| **Origin** | a closed source sum (corpus / web / attachment / tool-output) + which machine served it + grain (leaf/summary). Every field required. |
| **Custody** | where this content stands, carried by the chunk, stamped at retrieval. |
| **SharingPolicy** | what a corpus permits the mesh: Local, or Mesh with queryable/replicable. Corpus-level by design. |
| **Seal** | the corpus scope a turn is bound to, fixed at retrieval. |
| **Verdict** | passed / failed / could-not-judge / never-ran. Four states, one definition. |
| **Judgement** | a verdict plus its evidence quote and the attribution of the judge that produced it. Mintable only by a calibrated judge. |
| **Attribution** | which engine computed this text: model, build, quantization, host. One type, four consumers. |
| **Gap** | a demand for evidence the corpus cannot meet — the deep-research loop's round-driving input. |
| **Answer** | text + citations + provenance + judgement. An honest abstention is an `Answer`. |
| **Capability&lt;T&gt;** | `Available(T)` or `Unavailable(reason)`. Absence is a value. |
| **Measurement** | metrics plus the fingerprint of the conditions that produced them. |
| **Fingerprint** | lane + corpus + model + prompt version + sample cap + scorer version. The comparability key. |
| **Record** | an immutable fact with content-hash identity. Views are projections. |
| **Recipe** | the TOML declaring how a corpus is acquired, chunked, indexed and shared. |
| **Step / Artifact / Runner** | the one dataflow substrate — typed steps over cached artifacts. |
| **Profile** | a declared, validated set of capabilities for one deployment. |
| **Intent** | the router's verdict on a message, plus its row in the intent table. |

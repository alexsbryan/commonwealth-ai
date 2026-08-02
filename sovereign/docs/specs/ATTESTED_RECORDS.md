# Attested Records — a value cannot exist without its evidence

**Status:** design spec. Written 2026-07-31 against `main` @ 6acd23b9.
**Parent:** `VERIFICATION_COMMONS.md` (the verification strata and the
re-checkable-record carrier) and `VERIFIER_V0.md` (the trained verifier that
becomes stratum 5). Those docs verify *answers the runtime generated*. This
doc applies the same discipline to *records extracted from documents*, where
the output is not prose for a human to read but a typed row something
downstream will act on.

**The one-paragraph project.** Introduce one type — `Claim<T>` — whose
constructor requires a verbatim `Span` of a source document, and whose
verdict is computed by a verifier that is structurally forbidden from being
the extractor that proposed it. Around that type, a declared schema (TOML,
no Rust), a fitted accept threshold (measured against labeled examples, not
chosen), and an offline re-check that any third party can run against the
source bytes months later. The codebase has already grown five incompatible
partial versions of this idea; this collapses them into one and gives every
document-to-record vertical — contract terms, invoice lines, protocol
endpoints, compliance matrices — the same substrate.

---

## 0. Why this exists: five vocabularies for one idea

Every one of these is a real, live, partial answer to "how do I know where
this value came from and whether it's right":

| Where | Evidence | Verdict | Gap |
|---|---|---|---|
| `investigation/graph.rs:66-97` | `Evidence { chunk_id, excerpt }`, **required** | `confidence: f32` | Self-reported. Code says *"Free-form for now; the test harness asserts shape only"* |
| `atlas/atoms.rs:355-390` | `Provenance { extractor_id, source_doc_id, source_chunk_id, signal_kind }` + `ChunkRef` | none | Per-atom, not per-field. Section-grain, no offsets |
| `epistemic.rs` | `Holding { claim, provenance, verification }` | `Verification { Verified \| FailedOnce \| FailOpen \| Unverified }` | Best verdict vocabulary in the tree; `chunk_id` is `None` at every write site |
| `document.rs:522-534` | `QuoteSpan { chunk_id, char_start, char_end, text }` | none | Real offsets, attached only to RAPTOR nodes |
| `meshapp/wrapped.rs` | `Citation { chunk_id, char_start, char_end }` | re-verified at serve time, `Err` = do not serve | The strongest of the five. Scoped to one mesh feature |

Five teams-worth of the same insight, none composable with the others. An
abstraction a codebase reinvents five times independently is not speculative
— it is unnamed. This spec names it and deletes the other four.

The one that got it right is `wrapped.rs`: build-time verification *and*
serve-time re-verification against the live index, refusing to serve a
failing artifact. Generalizing that from one feature to a system contract is
the whole move.

---

## 1. The abstraction: four invariants

Not a pipeline. Four rules about what a valid extracted field *is*.

**I1 — The schema is data.** Fields, types, units, and normalizers are
declared in TOML and resolved at runtime into a decoding grammar. A domain
expert adds a field without an engineer and without a rebuild. Extends the
existing `EntityTypeDecl` path (`recipe.rs:764`), which already proves the
declaration → JSON Schema → llguidance chain works.

**I2 — A value without a span is unrepresentable.** Not discouraged —
unconstructible. `Claim::new` takes a `Span` by value; there is no
constructor that omits it. `ARCH_PRINCIPLES §7.1`: encode the invariant so
it cannot be forgotten. An unsourced field cannot leak into a record because
there is no way to build one.

**I3 — The verdict is computed by something other than the extractor.**
`Verifier::independence()` is a required method, and the pipeline refuses to
mark a field `Accepted` when the verifier shares a model with the extractor
that proposed it. This is the load-bearing rule. The measurement that
justifies it is already in this tree: the weak judge returned 0.42–0.76 on
*known fabrications* where the strong one returned 0.96–0.98
(`runtime/grounding/judge.rs:51-55`). A model grading its own output is not
a signal; making that structurally impossible is the difference between this
and every extraction product that ships a `confidence` float.

**I4 — The accept threshold is fitted, never chosen.** `attest fit` sweeps τ
per field against labeled examples and reports the operating point:
*at τ=0.82, auto-accept covers 78% of fields at 97.4% precision*. A field
with too few gold examples reports `Uncalibrated` and defaults to review —
following the repo's existing rule that an empty population is never a
passing score (`lane_baseline.rs:205`, the zero-test-run gate).

Everything else — which model extracts, which OCR engine ran, whether the
verifier is a cross-encoder or a logprob read — is a swappable
implementation detail *inside* these four rules.

---

## 2. The types

```rust
/// sha256 of the source document's raw bytes. Minted by the existing
/// content-addressed asset store (`corpus-engine/src/asset_store/`),
/// which already keeps the bytes and an append-only ledger.
pub struct DocId([u8; 32]);

/// The anchor. `section_path` is the human-facing citation ("§7.3",
/// "Exhibit A §2(b)"); the char range is the machine one. `text` is
/// retained verbatim so a re-check can run offline against the bytes
/// without re-parsing the document.
pub struct Span {
    pub doc: DocId,
    pub page: Option<u32>,
    pub section_path: Option<String>,
    pub char_start: u32,
    pub char_end: u32,
    pub text: String,
}

/// Concrete model stem, never a slot alias — the lesson already paid for
/// in `lane_baseline.rs:137` and the RAPTOR provenance stamping.
pub struct Attribution {
    pub extractor: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub extracted_at: i64,
}

pub enum Verdict {
    Supported   { p: f32, by: VerifierId },
    Unsupported { p: f32, by: VerifierId },
    /// Deterministic rejection — the value does not parse from its own
    /// span, or the span is not verbatim in the source. This is a bug,
    /// not a low-confidence extraction, and it costs zero model calls.
    Invalid     { reason: InvalidReason, by: VerifierId },
}

pub enum Disposition {
    Accepted,
    NeedsReview(ReviewReason),
    Withheld(WithholdReason),
}

/// Construction requires a Span. There is no other constructor.
pub struct Claim<T> { /* private */ }

impl<T> Claim<T> {
    pub fn new(value: T, span: Span, attribution: Attribution) -> Self;
    pub fn value(&self) -> &T;
    pub fn span(&self) -> &Span;
    pub fn verdict(&self) -> Option<&Verdict>;
}
```

**The absence case gets a type of its own**, because "we did not find it" and
"we did not look" must not render identically — that distinction *is* the
customer requirement:

```rust
pub enum FieldOutcome<T> {
    Found(Claim<T>),
    /// Carries the spans that WERE searched. An absence with an empty
    /// `searched` set is itself a review trigger, not a clean negative.
    Absent { searched: Vec<Span>, disposition: Disposition },
    /// Two spans, incompatible values. Never silently resolved — a
    /// contract that says 30 days in §7 and 60 days in Exhibit B is a
    /// finding, not a tie to break.
    Conflicting { candidates: Vec<Claim<T>> },
}

pub struct Record {
    pub doc: DocId,
    pub schema_version: String,
    pub fields: BTreeMap<FieldId, FieldOutcome<Value>>,
}
```

`Conflicting` is deliberate. Every extraction product silently picks one.
Surfacing the conflict with both spans is more valuable than either answer,
and it is the case a human reviewer most wants routed to them.

---

## 3. Two traits, and the verifier strata

```rust
pub trait Extractor {
    fn id(&self) -> ExtractorId;
    fn propose(&self, doc: &PreparedDoc, field: &FieldSpec) -> Result<Vec<Candidate>>;
}

pub trait Verifier {
    fn id(&self) -> VerifierId;
    /// Enforces I3. The pipeline refuses `Accepted` when this returns
    /// `SharesModelWith(the proposing extractor)`.
    fn independence(&self) -> Independence;
    fn verify(&self, span: &Span, field: &FieldSpec, value: &Value) -> Result<Verdict>;
}
```

Verifiers run cheapest-first and short-circuit. The ordering is the cost
model:

| Stratum | Implementation | Cost | Reuse |
|---|---|---|---|
| 1. Span integrity | Is `span.text` verbatim in the source at that range? | microseconds, deterministic | `quote_verification.rs` — already this, already hardened against five real false-positive classes |
| 2. Normalization | Does `value` parse from `span.text` under the declared normalizer? ("thirty (30) days" → `30`) | microseconds, deterministic | `numeric_audit.rs` — already this, 1e-6 tolerance, "the model must never originate a number" |
| 3. Entailment | Cross-encoder NLI, ~400M params | milliseconds | new; the lean default |
| 4. Forced choice | Single-token A/B logprob read | one forward pass | `x_forced_choice` (`completion.rs:332`, `model_slot.rs:680`) — built, unused for this |
| 5. Trained verifier | Qwen3.5 grounding verifier | one forward pass, better calibrated | `VERIFIER_V0.md` when it lands |
| 6. Human | Review queue | minutes | its verdicts become gold-set rows — the flywheel |

**Strata 1–2 can reject on their own authority.** A value that does not parse
from its own span is `Invalid`, not low-confidence, and dies for free. This
is expected to catch a large share of extraction errors before any model
runs — and it is the part that needs no GPU, which makes milestone M0
testable under `ARCH_PRINCIPLES §12.4`.

---

## 4. The declaration surface

Three files. This is the entire application-authoring API.

```toml
# schema.toml
schema_version = "contract-terms/1"

[[field]]
id          = "auto_renew_notice_days"
type        = "integer"
unit        = "days"
normalizer  = "duration_days"      # "thirty (30) days" -> 30
prompt      = "The number of days' notice required to prevent automatic renewal."
locate      = ["renewal", "term", "notice"]   # embedding hints for stratum 0 retrieval
required    = true

[[field]]
id          = "liability_cap_usd"
type        = "money"
normalizer  = "usd"
prompt      = "The maximum aggregate liability, in USD."
required    = false
```

```jsonl
// gold/acme-msa.jsonl — one line per labeled field
{"doc":"sha256:…","field":"auto_renew_notice_days","value":30,"span":{"section_path":"7.3","char_start":14022,"char_end":14119}}
```

```toml
# policy.toml
[disposition]
accepted_at      = "fitted"        # never a hand-picked number
uncalibrated     = "needs_review"  # too few gold examples -> a human looks
conflicting      = "needs_review"
absent_required  = "needs_review"
invalid          = "withheld"      # deterministic failure never ships
```

Three commands:

```
svrn attest fit    --schema schema.toml --gold gold/
svrn attest run    --schema schema.toml --in docs/ --out records/
svrn attest verify --record records/acme-msa.json
```

---

## 5. Calibration: `attest fit`

Per-field τ, not global. "Governing law" is nearly free; "liability cap" is
hard. One threshold across both wastes coverage on the easy field and
precision on the hard one.

Output is an **operating point** — the artifact a buyer actually signs off
on:

```
field                     n_gold   τ      coverage   precision   status
auto_renew_notice_days      184   0.79      0.91       0.983     calibrated
liability_cap_usd           171   0.88      0.64       0.971     calibrated
fee_schedule_lines           92   —         —          —         uncalibrated (tables)
governing_law               190   0.61      0.98       0.996     calibrated
─────────────────────────────────────────────────────────────────
aggregate                          —        0.78       0.974
```

Gated the same way every other lane in this repo is gated: the operating
point is a `LaneBaseline`, regression-diffed by `bench_cmd/gate.rs`, so a
prompt change that quietly costs three points of precision breaks the build
instead of shipping.

Two rules inherited from existing gates, both load-bearing:

- **An uncalibrated field is never `Accepted`.** Fewer than N gold examples
  → `Uncalibrated` → review. Silence is not success.
- **Coverage and precision never blend into one score.** The chaos harness's
  two-red-line rule (`bench/chaos_monkey/`): a blanket-abstainer and a
  hallucinator must both be visibly bad, which a single blended number
  permits them to hide behind.

---

## 6. The re-check contract: `attest verify`

**This is the product.** A record that a skeptical third party — an auditor,
the customer's finance team, opposing counsel — can independently re-verify
against the source bytes months later, without trusting the vendor, the
model, or the pipeline that produced it.

```
svrn attest verify --record records/acme-msa.json
  → resolves each Span.doc against the asset store by sha256
  → re-runs stratum 1 (span verbatim at range) and stratum 2 (value parses)
  → exit 0, or the list of broken bindings
```

**It deliberately does not re-run strata 3–6.** A re-check must be
deterministic, offline, and reproducible on a machine with no GPU and no
network, or it is not a check — it is a second opinion. Model strata produce
the verdict once, at extraction time, stamped with `Attribution`; the
re-check confirms the *binding* survived, which is the property that actually
degrades (documents get re-versioned, records get hand-edited, spans go
stale).

Signing, when a record must cross a trust boundary, reuses
`VERIFICATION_COMMONS.md §6` — Ed25519 over domain-separated canonical
bytes, content-hash identity. **No new identity scheme.**

---

## 7. Reuse ledger

Nine of the twelve components exist. This spec is mostly consolidation.

| Need | Existing component |
|---|---|
| `DocId` + raw-byte retention | `corpus-engine/src/asset_store/` — sha256-addressed, append-only ledger |
| Stratum 1 | `sovereign-core/src/quote_verification.rs` |
| Stratum 2 (money/percent) | `sovereign-core/src/runtime/numeric_audit.rs` |
| Stratum 4 | `x_forced_choice` → `forced_choice_probs` |
| Stratum 5 | `VERIFIER_V0.md` |
| Constrained proposal | `llguidance` via `CompletionRequest.structured_output` |
| Schema → grammar | `investigation/extract.rs:182 response_schema` |
| Fit harness scoring | `sovereign-eval` B³/pairwise-F1 + `harness/field_coverage.rs` |
| Operating-point gating | `bench_cmd/lane_baseline.rs` + `gate.rs` |
| Batch runner | `sovereign-workflow` `for_each` + `on_error = "skip"` + per-element failure records |
| Signing/carrier | `VERIFICATION_COMMONS.md §6` |
| Locate stage | `qwen3-embedding-0.6b`, brute-force over one document's clauses — no ANN index |

**New:** the `Claim`/`Span`/`Verdict` types, the two traits, the entailment
verifier, the fit command, and clause/page anchoring (see §10).

---

## 8. Deletes ledger

Per the funding rule: a plan is worth its complexity only if it net-removes.

**Concepts collapsed, five → one:** `investigation::Evidence` +
`atlas::Provenance`/`ChunkRef` + `epistemic::Holding`/`Provenance` +
`document::QuoteSpan` + `meshapp::Citation` all become `Span` +
`Attribution` + `Verdict`. Two of those (`quote_verification`,
`numeric_audit`) survive as `Verifier` impls rather than freestanding
special cases.

**Removed from the document-to-record path** (they remain for corpus-scale
knowledge work; they are simply not in this pipeline):

- RAPTOR trees — hierarchical summarization answers a question a term sheet
  does not ask.
- The atom graph / atlas / tiered enrichment — cross-document entity
  resolution solves a problem a per-document record does not have.
- The investigation pipeline's coalesce phase — merges mentions into a
  corpus-wide graph; the output wanted here is a row.
- LanceDB + Tantivy + IVF-PQ — corpus infrastructure. Forty clause
  embeddings is a dot product.
- The chat runtime and grounding gate — built for conversational answers
  with retry and rewrite ladders. Batch extraction wants a verifier, not a
  conversation.

**Knobs:** zero new environment flags. Configuration lives in the schema and
policy TOML, which is versioned with the data it governs. (`quality/env-flags.toml`
stays untouched, so `cargo xtask env-gate` has nothing new to admit.)

---

## 9. Non-goals

1. **No knowledge graph.** Records are per-document and independent. Linking
   them is a downstream concern with its own spec.
2. **No cross-document entity resolution.** "Is the Acme in this MSA the
   Acme in that order form" is out of scope.
3. **No conversation, no summarization.** The output is a row.
4. **No new identity, signing, or transport scheme.** Defers to
   `VERIFICATION_COMMONS.md §6`.
5. **No model training here.** Stratum 5 defers to `VERIFIER_V0.md`.
6. **No acquisition.** Documents arrive; getting them is the caller's job.
7. **No auto-resolution of conflicts.** `Conflicting` is a finding.

---

## 10. Risks

**Span anchoring depends on document plumbing that does not exist.** A
`Span` is only as good as its anchor, and the current attached-document
chunker is a blind 700-char sliding window (`rag/chunk.rs:28-29`) — the best
citation available today is "chunk 41," not "§7.3, p.12." Clause
segmentation and page anchoring are a hard prerequisite, not a polish item.
*Mitigation:* M0 ships with `section_path: None` and char offsets only,
which is already re-checkable; the human-facing anchor lands in M3.

**Tables.** Flat character-stream PDF extraction interleaves table cells,
and the highest-value fields (fee schedules, discount tiers) are tables.
Nothing in-tree recovers cell topology. *Mitigation:* the schema marks such
fields, `attest fit` will report them `Uncalibrated`, and they route to
review rather than shipping wrong. The failure is visible and safe, which is
the whole design; it is still a failure.

**Verifier independence is easy to violate accidentally.** A cross-encoder
fine-tuned on the extractor's outputs is not independent even though
`independence()` would report that it is. *Mitigation:* `Independence` must
be derived from training provenance, not declared by hand, once stratum 5
exists.

**Constrained decoding currently fails open.** On grammar-compile failure
the sampler warns and falls back to free-form sampling
(`sampler.rs:304-337`). For this use case that must be fail-closed — an
unconstrained extraction that looks like a constrained one is exactly the
silent corruption this spec exists to prevent. *This is a prerequisite fix,
and it is small.*

**Gold-set volume.** Per-field τ needs enough labeled examples per field.
Rare fields will sit `Uncalibrated` for a long time. That is honest, and it
is also a coverage ceiling the customer must be told about up front.

---

## 11. Milestones

**M0 — the type and the free strata. No model calls.** `Span`, `Claim`,
`Verdict`, `FieldOutcome`, `Record`; `Extractor`/`Verifier` traits; strata 1
and 2 wired from `quote_verification` and `numeric_audit`. Ships with a
deterministic test suite that needs no GPU, no network, and no weights.
*Done when:* a hand-authored `Record` round-trips and `attest verify`
catches a tampered span.

**M1 — `attest fit` and the operating point.** Schema TOML, gold JSONL, τ
sweep, per-field coverage/precision report, `LaneBaseline` gating.
*Done when:* the report refuses to certify an under-sampled field.

**M2 — the model strata.** Entailment cross-encoder (stratum 3) and the
forced-choice path (stratum 4), with `independence()` enforced.
*Done when:* a run with extractor == verifier cannot produce `Accepted`.

**M3 — anchoring and the review queue.** Clause segmentation, page numbers,
`section_path` populated; the review surface, whose human verdicts write
back as gold rows.
*Done when:* a record cites "§7.3, p.12" and a reviewer's correction appears
in the next `fit`.

**M4 — the contract vertical, as proof.** The schema, gold set, and policy
for SaaS contract terms — three files, no Rust.
*Done when:* the second vertical costs three files and no code.

---

## 12. Open questions

1. **Is stratum 3 (cross-encoder) or stratum 4 (forced choice) the better
   default?** Stratum 3 is ~50× cheaper; stratum 4 needs no new dependency
   and reuses a measured primitive. Decide on M2 data, not taste.
2. **Should `Absent` require a search receipt?** Recording *which* spans were
   examined makes absence auditable but multiplies record size. Probably yes
   for `required` fields, no otherwise.
3. **Per-field τ vs. per-(field, document-class) τ.** A born-digital PDF and
   an OCR'd scan almost certainly warrant different thresholds for the same
   field. Defer until M1 shows the spread.
4. **Where does `Record` live?** SQLite alongside `document_assets` is the
   obvious host, but records should be exportable and re-checkable without
   the daemon. Leaning toward JSON-on-disk as canonical with SQLite as an
   index.

---

# Addendum A — pitching the contract vertical to a working engineering team

**Non-normative.** This section is sales strategy, not a system contract.
Nothing here is an assertion about the code; §0–§12 above are.

**Audience:** the founders and engineering leadership of a startup that
already runs a single-pass contract-extraction pipeline in production,
feeding terms into budgeting software. They ship. They are rightly
suspicious of replatforming.

## A.1 The one thing to lead with

Not accuracy. Not the architecture. This:

> *Which of the terms you shipped last month were wrong — and did you find
> out before your customer did?*

Their extractor is probably fine. The problem is that they cannot **rank**
its outputs by trustworthiness, so every field costs the same review
minute regardless of whether it was trivially obvious or genuinely
ambiguous. That is the whole cost structure, and it is invisible from
inside the pipeline.

The product is not better extraction. It is a **sortable, calibrated
confidence** that lets them stop reviewing the 78% that are provably fine.

## A.2 Do not pitch the abstraction

Pitching "attested records" to a working startup makes them evaluate a
*framework adoption* decision, which is a far higher bar than what we
actually want them to say yes to. It also invites the correct objection
that they don't need a platform, they need one workflow to cost less.

Pitch the contract measurement. Build the abstraction underneath it. The
second vertical costing three TOML files is a fact we discover together
later, not an argument we make in the first meeting.

## A.3 The migration story is additive — say so in the first five minutes

This is the objection that kills the deal if it goes unanswered, so answer
it before it is asked.

**Phase 0 runs alongside their pipeline. Nothing gets turned off.** We take
their existing extractor output, attach spans and verdicts to it, and
measure. If the operating point is bad, they have spent two weeks and
learned their own true field-level error rate — which they do not currently
have and which is worth having regardless of whether they ever buy anything.

That reframes the ask from "replace your system" to "instrument your
system." Engineers say yes to instrumentation.

## A.4 Lead with the broken parts

This audience trusts people who name their own gaps first, and they will
find these anyway.

- **Tables are not solved.** Fee schedules and discount tiers are tables;
  flat PDF extraction interleaves them. Those fields will come back
  `Uncalibrated` and route to a human. The failure is visible and safe. It
  is still a failure, and it lands on some of their highest-value fields.
- **Span anchoring needs building.** Today the best anchor is a chunk
  ordinal, not "§7.3, p.12." M0 ships char offsets only.
- **Constrained decoding currently fails open** (§10). We are fixing it
  before the pilot; they should know it was there.
- **Model weights, not code.** `deny.toml` gates Rust crate licenses in CI
  against an AGPL-compatible allow-list; it does **not** gate GGUFs. Every
  shipped model — extractor, verifier, OCR, embedder — needs its own
  license check. `VERIFIER_V0.md §0` already carries the precedent:
  Bespoke-MiniCheck-7B is CC-BY-NC-4.0, *"commercial use requires a
  negotiated license… never shippable."* This is a checklist, not a
  blocker, and it must be done before the engagement, not after.

AGPL itself is **not** on this list. See §A.9.

## A.5 The demo is thirty seconds and needs no GPU

Not a chat window. Not the desktop app. This:

```
$ svrn attest verify --record records/acme-msa.json
✗ liability_cap_usd — span no longer resolves in source
    §12.1, chars 41022..41118
    expected: "aggregate liability shall not exceed $2,400,000"
    found:    "aggregate liability shall not exceed $4,200,000"
exit 1
```

Hand-edit the source document, re-run, watch it fail. That is the entire
thesis in one command: *a record that checks itself against its own
evidence, offline, by someone who doesn't trust you.* It is also the M0
deliverable, so the demo is real on day one rather than a mock.

**Do not demo:** the mesh, RAPTOR, the atom graph, the chat runtime, the
desktop app. Everything in the §8 deletes ledger is also on the do-not-show
list. Breadth reads as unfocused to a team evaluating a narrow tool.

## A.6 Objections, with honest answers

| They say | Answer |
|---|---|
| "We already emit confidence scores." | Self-reported ones. Ask what their precision is at their current accept threshold. They won't know — the threshold was chosen, not fitted. That gap *is* the pitch. |
| "Structured outputs already give us typed JSON." | Structured output constrains the *shape*, not the *truth*. Nothing in a JSON schema asserts the value came from the document. |
| "Models keep getting better; this is a wrapper." | The verification layer is orthogonal to model quality. A better extractor raises coverage at fixed precision; it still cannot tell you which fields to check. Better models make this *more* valuable, not less — coverage goes up, the triage stays necessary. |
| "This is a Rust monolith we'd be adopting." | Phase 0 is a sidecar over their existing output. Adoption decisions come after the number. |
| "Our accuracy is already 95%." | On which fields, measured against what, and at what review cost? 95% aggregate hides a 60% field. Per-field is the whole point. |

## A.7 What to ask them for

Two numbers decide everything, and both are theirs:

1. **200 contracts with the terms they already extracted and corrected.**
   That is the gold set. No labeling project — they have this as a
   byproduct of their current review step.
2. **Current per-contract human review time**, and what fraction of fields
   a human touches. If the honest answer is "all of them," the arithmetic
   writes itself.

## A.8 The ask

A two-week paid pilot whose sole deliverable is the §5 operating-point
table computed against their own contracts and their own labels. Bounded,
cheap, and the artifact is valuable to them whether or not they buy
anything afterward.

If the table shows 78% coverage at 97% precision, the business case is
arithmetic. If it shows 40% coverage, we have learned that their document
mix is harder than assumed and we say so — which is the version of this
that earns a second meeting.

## A.9 AGPL is a pricing lever, not an obstacle

Not legal advice; get counsel on the actual engagement terms. But the
mechanics are settled and the repo already anticipated them.

**The deploy-and-service model needs no special arrangement.** AGPL permits
commercial use and the sale of services. Deploying the system for a
customer, charging a fee, and letting them run it is precisely what Red Hat
does with GPL software. The only obligation to the recipient is the source
(already public) and the license text.

**Dual licensing is already architected in.** `CLA.md` uses the Harmony
agreements, under which contributors grant the maintainer the right to
sublicense and relicense. Its own plain-language summary says the purpose
out loud: *"This is what lets the project be offered under more than one
license, for example a paid commercial license alongside the public AGPL
one."* The public license binds downstream recipients, not the copyright
holder. The reciprocal promise (§2.3 — the project always stays available
under an OSI license) is a constraint on going closed, not on selling.

**The one place AGPL genuinely bites is on their side, not ours.** §13:
if *they* modify the system and expose it to *their* users over a network,
they owe those users their modified source. For a company reselling
contract extraction as a service, that is a live question — it is the exact
scenario the license exists to catch. Two clean outs, both already
available:

1. Run it unmodified → no §13 obligation at all.
2. Buy a commercial license → the CLA already permits granting one.

That is a conversation worth *having*, not avoiding. It is the moment a
services engagement becomes a licensing one.

**The durable revenue is recalibration, not hosting.** The §5 operating
point decays: every schema change, every shift in their document mix, every
model swap invalidates it and requires a re-fit against fresh labels. That
is real, recurring, measurable work with an artifact at the end — a far
better subscription than renting them compute they could buy for $2,500.

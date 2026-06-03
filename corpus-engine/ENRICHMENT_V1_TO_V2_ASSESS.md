# Enrichment v1 → v2 Migration — Assess Deliverable (PR7)

_Spike-before-commit assessment. Decides whether the v1 `Domain`
(field-engine) surface can fold into the v2 `Pipeline` (atlas) surface,
what the coverage gap is, and a phased path. No migration is performed
here — this is the gate the plan asks for before touching code._

Status: **assessment only.** Recommendation at the bottom. Author: PR7
of the tech-debt program (2026-06-03).

---

## The question (verbatim from the plan)

> Can v1 `field_engine` KnowledgeView domains express on the v2
> `Pipeline` / §5.4 corpus-free builders? What's the test-coverage gap?

### Correction to the question's premise

Two parts of the framing are inaccurate against the code as it stands;
the assessment is more useful once they're fixed:

1. **"§5.4 corpus-free builders" does not exist.** `ENRICHMENT_V2.md`
   is "Enrichment Atlas v2.2 — Plan of Record" and is organised by
   *Landings* (§ "Status", "Landing 2", "Landing 5", "Phase A/C"), not a
   numbered §5.4, and neither "corpus-free" nor a "builder" abstraction
   appears in it (`grep -i 'corpus.free\|builder\|5\.4'` → only an
   unrelated `HdbscanHyperParams::builder`). The reference is stale —
   likely from an earlier spec draft. **Action:** drop it from the plan,
   or point it at a real artifact. The migration does not depend on a
   "corpus-free builder" that isn't there.

2. **"KnowledgeView domains are SQLite-sourced" is true but irrelevant
   to the enrichment layer.** KnowledgeView (`personal` → `memories`,
   `conversational` → `conversations`+`messages`, `institutional` →
   agent working-notes) sources rows from SQLite *at the acquirer /
   recipe layer*. By the time enrichment runs, the input is the same
   `&[&Chunk]` every other corpus produces. So "can a row-sourced domain
   run on a chapter-oriented pipeline?" is a non-issue: both v1 `Domain`
   and v2 `Pipeline` consume corpus chunks, not table rows. The
   SQLite-vs-document distinction lives upstream of the surface we're
   unifying.

So the real question reduces to: **can a v1 `Domain` (5-phase) be
re-expressed as a v2 `Pipeline` (7-phase atlas), and is the result the
same knowledge product?**

---

## The two surfaces, precisely

| | v1 `Domain` (`enrichment/domain.rs:26`) | v2 `Pipeline` (`enrichment/pipeline/trait_def.rs:41`) |
|---|---|---|
| Input | `&[&Chunk]` | `ChapterInput` (per-chapter) |
| Phases | 5: skeleton → cluster → fault-lines → open-questions → entities | 7: question-extract → cluster → concern-name → chunk-cluster → position-extract → tension-detect → gap-detect |
| Prompts | inline `fn …_prompt(&[&Chunk]) -> String` | `phaseN_system()` from `include_str!` assets + `compose_phaseN(input, exemplars)` + `parse_phaseN()` |
| Extras | clustering/alignment/fault-line configs | seed strategies (`SeedStrategy::{None,Llm,Structural}`), exemplar bank top-K, typed JSON schemas, validation |
| Output | skeleton + fault-lines + open-questions + entities | typed atom graph (questions / positions / tensions / gaps) |
| Dispatch | `domain_registry.rs` (11 factories) | `pipeline/registry.rs` (5 pipelines) |

The trait doc says it outright: *"`Pipeline` is a sibling of the v1
`Domain` trait, not an extension."* They are two different knowledge
products, not two implementations of one. v2 is richer (seeds,
exemplars, typed schemas, tension/gap phases) and is where all new
investment goes.

---

## Coverage gap (the real blocker)

Domain inventory by registry:

- **v1 `Domain` (11):** philosophy, science, policy, legal, community,
  multi, engineering, personal, conversational, business_email,
  institutional.
- **v2 `Pipeline` (5):** philosophy_atlas, engineering_atlas,
  conversation_atlas, literary_atlas, referential_atlas.

Cross-tabulated:

- **Both surfaces (3):** philosophy, engineering, conversational — these
  can be A/B'd today (a v1 domain and a v2 pipeline both exist).
- **v1-only, no v2 atlas (8):** science, policy, legal, community,
  multi, personal, business_email, institutional. Migrating these means
  *authoring a new pipeline* (7 phases of prompts + typed schemas +
  exemplars), not porting one.
- **v2-only (2):** literary, referential — born in v2, never had a v1
  domain.

So a "fold v1 into v2" is **3 re-expressible + 8 net-new authoring +
2 already-done**. The headline cost is the 8 net-new pipelines, each of
which is a prompt-engineering + eval-tuning effort comparable to the SEP
/ literary atlas campaigns, not a mechanical port.

---

## Test-coverage gap

- **v2 has the harness.** `sovereign bench all` rolls up
  enrichment-eval atom-F1 per pipeline; baselines live under
  `baselines/<bench-id>/latest.json`. Any pipeline migration has a
  ready F1 parity gate.
- **v1 domains are unevenly covered.** The overlapping three
  (philosophy/engineering/conversational) have benches; several v1-only
  domains (policy, legal, community, business_email, institutional) have
  thin or no enrichment-eval coverage — so "did the migration preserve
  quality?" has **no baseline to compare against** for those. That is
  the binding test-coverage gap: you cannot gate a migration you can't
  measure.
- **Consumer audit missing.** v1 KnowledgeView outputs
  (skeleton/fault-lines/open-questions) are read by desktop surfaces and
  the timeline assembler; v2 atoms are read by the atlas traversal /
  brief assembler. Before flipping a domain, its *downstream consumers*
  must be confirmed to accept atoms in place of the v1 product. This is
  unaudited and is a correctness gap, not just a quality one.

---

## Recommendation — phased, gated, do NOT migrate blind

1. **Fix the plan's stale reference** (drop "§5.4 corpus-free builders").
2. **Prove subsumption on one overlapping domain first.** Pick
   `conversational` (highest-traffic KnowledgeView; both surfaces exist).
   Run the v1 domain and `conversation_atlas` over the same corpus and
   diff outputs against the *consumers'* needs — does the atom graph
   carry everything the v1 skeleton/open-questions fed downstream? This
   is a measurement spike, ~1 session, no code flip.
3. **Gate the flip on F1 parity + a consumer-acceptance check**, behind
   a recipe flag (per `feedback_prompt_overlay_dir` / recipe config), so
   it's reversible. Migrate exactly one surface (institutional was the
   plan's suggested first; conversational is the better-instrumented
   choice).
4. **Do not author the 8 net-new pipelines** until step 2 proves the
   atlas product subsumes the v1 product for at least one domain. If it
   doesn't subsume (likely for the row-oriented KnowledgeView trio,
   whose value is timeline/entity recall, not question/tension/gap
   atoms), the correct outcome is **keep v1 `Domain` for those and stop
   calling it debt** — two surfaces is fine when they produce two
   genuinely different products.

**Bottom line:** the surfaces *can* coexist on the same chunk input, but
v2 is not a drop-in for v1 — it's a different, richer knowledge product
missing 8 of v1's 11 domains and lacking baselines for the un-benched
ones. The honest first step is a one-domain subsumption + consumer spike,
not a migration. The "consolidation" framing should be downgraded from
"refactor" to "product decision per domain."

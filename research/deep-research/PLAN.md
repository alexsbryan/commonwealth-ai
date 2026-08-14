# Deep research — the adversarially synthesized engineering plan

**Status:** Seat synthesis, 2026-08-13. Derived from `sovereign/docs/specs/DEEP_RESEARCH.md`
(design draft, same day) put through the mesh-scale §7 treatment: one reviewer verifying
every reuse citation and precondition against HEAD, one briefed to refute the design and
the measurement plan. Operator intake: note `38bc1862`. Reviewer reports are summarized
here; every load-bearing claim carries the citation the reviewer actually checked.

**The operator's charter for this document:** an engineering plan where every stage has a
measurement that FAILS today (or at a named stage) and passes when the feature reaches
fruition. That is the mesh-scale red-first discipline, and this plan is organized around
its red table (§3) rather than around the component roster.

## 1. Verdict summary

The thesis **survives** — a research question spawning a persistent, gated corpus, with
frontier models as gated generators behind a custody boundary, is buildable largely from
surfaces that exist. The trust-class razor is the right instrument. But the mesh-scale
pattern repeated exactly: the diagnosis survives and the prescriptions needed surgery.

| Spec element | Verdict | One line |
|---|---|---|
| Thesis + operator journey | SURVIVES | All six journey scenes are satisfiable local-only — which is the smallest-v1 argument. |
| Trust-class razor | SURVIVES | Correctly names this repo's characteristic failure; misapplied in three places below. |
| Containment corollary ("no model output is control flow") | **REFUTED** | R1's plan IS the work list: it decides what runs, what is searched, and the denominator of the synthesis gate. |
| R1 Director as Class G / frontier | **REFUTED as stated** | A plan of three easy sub-questions clears a 100% coverage floor and mints a complete-looking answer to the wrong question. G only if the report's completeness claim is scoped to the enumerated sub-questions and the floor is charter-time absolute. No frontier in v1. |
| R5 Triage as Class G | **REFUTED as stated** | A systematic skip (a source class, a viewpoint) yields a self-consistent pool R3 is structurally blind to. Fix restoring G: R5 ranks, never excludes; code-set K + ε-quota of below-cut fetches; skip-ledger is an ICD. |
| R7 mitigations (faithful-mode + derived-tagging) | **REFUTED as sufficient** | (i) unknown provenance defaults to Leaf/primary — unsafe direction for a corpus that is majority-derived (`grounding/mod.rs:248-253`); (ii) "faithful-mode" as stated is a §7.6 violation unless respecified as post-hoc entailment by an I-class check; (iii) derived chunks have NO custody — the join rule over derivation inputs is real design work, not a schema tweak. |
| R10 "single choke point" | **REFUTED as stated** | An unguarded remote path ships today: `enrich extract` → Anthropic/OpenAI providers, zero privacy tokens in the file (`sovereign-cli-llm/src/enrich_cmd/providers.rs:63-66,214`). R10 is a fix to a shipped hole, and must absorb that path or the stance guarantee is scoped to one code path while an older one stays open. DDG search is also `SearchPrivacy::External` — queries cross the boundary before R10 is consulted. |
| "Search budget enforcement is C via BudgetView" | **REFUTED** | `BudgetView` has no writer anywhere; every production construction is empty → unlimited; `budget_allows` maps `None => true` (fail-open). A SECOND fail-open budget decider exists (`sovereign-tools-base/src/search.rs:234-260`, monthly, keyed "web"). Two implementations of one threshold before this feature adds a third. |
| FR-4 coverage floor | **REFUTED as specified** | A fraction of a model-chosen denominator is gameable by the generator it contains. Restate absolute: K of the charter's own named acceptance shapes, or N independently-sourced supported claims. |
| FR-6 dual-string | REVISED | Decorrelation is asserted, not measured — both strings are LLM calls in the same module, same model, same window (`judge.rs:491`, `mod.rs:107`). Measure agreement + joint-miss on a labeled set BEFORE it is load-bearing; until then single-string with the second as telemetry. |
| Precondition 1 (faithful enrichment) | REVISED | ~80% landed: summary verification exists, wired, default-on for corpus paths (`summary_verify.rs`, `raptor_atlas.rs:229-241`, `enrich_cmd/raptor.rs:375-386`). Residue: >~1500 nodes only Sample(0.12) is verified — research corpora live exactly there — and the auto-install hook doesn't thread `VerifyCtx`. |
| Precondition 2 (provenance through the gate) | **REFUTED at HEAD** | Landed as T1 P1.4: `EvidenceContext.chunk_sources`, Leaf/Summary, populated at all four production sites, default-on (`grounding/mod.rs:204-242,310-375`; env-flags:437). What is missing is custody + per-chunk source URL — precondition 2 collapses into precondition 3. |
| Precondition 3 (custody class) | CONFIRMED, but not "small" | `grep -ri custody` over the workspace: zero code hits. Enum + stamp sites + the derived-chunk join rule + egress semantics = the feature's real new surface. |
| FR-1/FR-2/FR-5, gym deck | SURVIVES | FR-2 (ICDs are the checkpoints) is the best idea in the spec; extend it to name spend meters and the R5 skip-ledger as ICDs. |
| Kill bar | **REFUTED as written** | "Beats cloud DR on neither honesty nor cost" ships on cost alone — and electricity wins cost by construction even for an empty report. Restated: ship iff P4 AND P2 AND P1. Cheapness is never a pass. |

**Spec errata (fix in the spec at first build order, §1.1):** every search/fetch path is
wrong — the code is `studio/crates/sovereign-tools-base/src/web/` (reachable as
`sovereign_tools::web` only via re-export; check `quality/ARCH_LAYERS.toml` before a
sovereign-side loop takes a studio dep). `KnowledgeLookupTool` has NO headless wiring
(desktop-only, double-defaulted-off; server registers `SearchTool::with_web` instead —
`main.rs:409`). `CorpusProvenance` is `{SelfInitiated, PeerPulled}` — ingest initiation,
not custody. launchd is the macOS half; this host is systemd. Mesh snapshots are
install-tarballs, not a query surface (`SearchPrivacy::Mesh` is an unused placeholder).

## 2. What the spec missed (prior art the plan now uses)

- **R3→R4 already shipped once**: `collaboration.rs:106` runs gap-check →
  `InformationRequest` → web-search, gated on `auto_collaborate`. The loop's compass has
  a working ancestor one layer above `KnowledgeLookupTool`.
- **Deterministic gap detection went 12/12** vs the LLM judge
  (`bench/gap_check/DECISION.md`, `cannot_know_from_here`). R3's detection half may need
  no model at all; reserve the judge for the audit half.
- **The web-rescue arc** (note `7721749e`) already enumerated four failure modes R8/R9
  would rediscover: evidence must be prepended under the audit's chunk-order cap,
  per-passage truncation, the date anchor, drafter padding thin snippets parametrically.
- **`EvidenceId` handles** (`knowledge_lookup/mod.rs:8-17`) are the citation-integrity
  primitive R9 needs — stable `ev-0001` ids the model cites, empty channels stay empty.
- **`DocumentAssetManager`** (attached-doc → verifier-gated RAPTOR tier) is the closest
  working analogue to R6+R7 over fetched sources.
- **The silent-success family is live in-tree** and is this feature's true adversary:
  ingest-unsearchable (note `89d5f75a`: `mark_indexes_built` never called on the
  workflow store path; retrieval's Filter 2 silently drops the corpus; CLI prints "✓
  searchable"); `merge-partitions` laundering a failed ingest (note `1ba77857`);
  enrichment `Ok` with a dead inference fn and zero atoms (note `5a56e565`).

## 3. The red table — every red measurable now, with its green

The operator's charter, as one table. "Instrument" names what runs; every row's red has
been verified to fail at HEAD by a reviewer (citations in §1) or is the documented
output of an existing harness.

| # | Red, failing today | Instrument | Green at fruition |
|---|---|---|---|
| R-1 | Workflow-ingested corpora are invisible to retrieval (`fanout corpora=0`; `needle-rig-baseline.sh` exits 4) | e2e: notebook ingest → assert `indexes_built` / fan-out count == installed | `mark_indexes_built()` on the workflow store path; repaired=0 |
| R-2 | `grep -ri custody` → zero code hits | grep + a failing unit: web-fetched chunk carries no custody/source-URL through `EvidenceContext` | custody enum + stamp sites + per-chunk URL reach the gate |
| R-3 | Unknown-provenance chunk grounds a factual claim (unknown→Leaf, `mod.rs:248-253`) | fixture: unstamped derived chunk in the window | third variant refuses; factual claims never rest on unknown provenance |
| R-4 | Mixed-custody tier-2 summary has no custody to key on | fixture through a stub egress check — fails by construction | derived custody = max-restrictiveness join over inputs, computed at creation |
| R-5 | A personal-corpus chunk can reach a remote payload (`enrich_cmd/providers.rs`, no privacy tokens) | unit: enrich payload from personal corpus vs remote dispatcher — nothing refuses | every remote client construction routes through the egress boundary; census enforced as a gate |
| R-6 | Three budget deciders, all fail-open, none run-scoped (`orchestrator.rs:222-230`, `search.rs:234-238`) | census + unit on `budget_allows(None)` | ONE decider, run-scoped, fail-closed, persisted as an ICD |
| R-7 | FR-6 decorrelation unmeasured | ~100 labeled claims through both strings; report agreement + joint-miss | number on record; FR-6 kept, dropped, or redesigned on it |
| R-8 | Faithfulness at scale: >1500 nodes only 12% of summaries verified; "Vladimir" case reproducible with `--verify-summaries off` | existing faithfulness bench lane (CI-wired, baselines committed) | unsupported-claim rate under baseline at the Sample regime + auto-install hook threads VerifyCtx |
| R-9 | P3 fetch-count: not observable (no counter, no writer) | build the meter, then search-gym fixture (§18.4: validate the meter first) | round-2 fetches <20% of round-1 AND coverage key not worse |
| R-10 | P4 coverage over the bank key = 0 by construction (no loop exists) | bank v0 coverage keys, scored by structured match (C, not a judge) | local-only arm clears the committed floor |
| R-11 | P5 drill cannot run (no state-transition ledger) | ICD-derived transition sequence; trace identity as arithmetic (clean = poisoned minus one round-block) | 100% on the deterministic mock deck |
| R-12 | Gap convergence never observed once | the T0 hand-run (below) | round-N gap set strictly shrinks on ≥X of 12 questions |

## 4. Tier structure

### T0 — instruments, preconditions, red baselines. No loop, no R11.

Contents: the hand-run compass experiment; production fix for R-1 (one
`mark_indexes_built()` call — the only production code in T0); custody schema +
join rule design (R-2/R-3/R-4 as failing tests = the specification); budget-decider
unification design note (R-6); FR-6 decorrelation measurement (R-7); bank v0 mint
(12 seeds + coverage keys + 12 adjacent + 6 repeat + 3 poisoned fixture dirs + the
~100-claim labeled set), numeric bars finalized in the mint commit before any arm runs
(§18.6); one timed end-to-end dry run to size the lane (if one run > ~20 min, bank
size and cadence are decided by arithmetic, not preference — the honest home is the
weekly tier, not `--quick`).

**The single riskiest assumption retires first, by hand, in half a day, zero new code:**
run 3 bank questions × 3 rounds using existing surfaces (`KnowledgeLookupTool` /
grounding gate + specifics scan / `SearchOrchestrator` DDG / manual triage). Is
round-2's gap set a strict subset of round-1's, and are the gaps phrased as things a
search query can act on? If no — the compass does not steer, and the spec's shape dies
here for the price of an afternoon, before R11 is written.

**Not worth continuing if:** the FR-6 strings show correlated errors (agreement high,
joint-miss ≈ single-string miss) — the "verified artifact" promise needs a redesigned
instrument before a loop is built around it. Or: the coverage key cannot be authored
without consulting system output — then the questions are wrong, not the harness.

**Collision note:** custody/provenance work lands in `runtime/grounding/mod.rs`, the
file the native-grounding cutover (BeefyMac seat) is actively editing. Land additive-only
(the P1.4 "empty = byte-identical" degradation pattern) or sequence behind that seat.
Do not open a second editor on that file.

### T1 — the local-only loop. No frontier, no egress boundary, no R10.

R0, R11-thin (typed states, ICDs, run-scoped lock — F19), R2 (+ estate-searchability
precondition assert, F16), R3 (+ empty-window = never-ran, F28's slot deadline), R4
(one budget decider), R5-as-ranker (F25), R6 (+ terminal-state poll, F17), R7 (+ R-3/R-8
mitigations), R8-local (URL constraint), R9-renderer (always flag, never remove;
`EvidenceId` handles). Gym deck over `MockBackendImpl` with F1-F28 injected.

Green targets: R-10 (P4), R-9 (P3 paired), R-11 (P5), R-12 (convergence), plus the
two-arm control: the loop vs a one-shot RAG answer on the same bank — the honest
"does the loop buy anything" measurement, measurable today, replacing the three-arm
design until T2.

**Not worth continuing if:** the local-only arm cannot clear the coverage floor after
loop iteration (the spec's own P4 clause, honored HERE, before any frontier work).

### T2 — egress + the one frontier role (R8 only).

R10 as the real choke point (absorbing `enrich --provider` — F26 census as a build
gate), custody proof over exact payloads, per-run consent UX (operator decision 2),
the P1 proxy arm (named as a proxy, never "cloud DR"), P2 between-arm with
pre-registered n and cluster-adjusted CI, restated kill bar: **ship iff P4 AND P2 AND
P1** — cheapness is never a pass.

**Not worth continuing if:** the restated kill bar fails.

Deferred past T2 entirely: R1-on-frontier (A/B once the bank exists), mesh sharing of
research estates (F27 foreign-embedding hazard precedes it; `SearchPrivacy::Mesh` is a
placeholder), R7 domain ontologies (no v1 metric).

## 5. FMEA extension

The spec's F1-F13 stand. The reviewers add F14-F28 — circular evidence (F14: synthesized
class, estate-visible, gate-INELIGIBLE), unstamped derived chunks failing closed (F15),
the silent-success family (F16 estate-unsearchable, F17 ingest laundering, F18 dead-
inference enrichment), run collisions (F19), budget-meter drift (F20), stale evidence
against a charter freshness horizon (F21), near-duplicate inflation — coverage counts
distinct origins, never chunks (F22), result-SET poisoning beyond P5's single plant
(F23), the mis-framed-plan gate pass (F24), systematic triage bias (F25), boundary
bypass census (F26), foreign embedding spaces in the estate (F27), instrument
unavailable ≠ could-not-judge (F28). The full rows with detection + response are in the
adversarial report; they enter the spec's table in the same commit that amends it.

## 6. Proposed bars — initiative `[deep-research]` (draft, operator re-cuttable)

1. `dr-compass` — R3's gap loop converges: T0 hand-run shows strictly shrinking,
   actionable gap sets (red: R-12 — never observed).
2. `dr-estate-integrity` — a research corpus built by shipped verbs is fully visible to
   retrieval, with ingest/enrichment failures loud (reds: R-1, F17/F18 asserts).
3. `dr-instrument-validated` — FR-6 decorrelation measured and the claim-gate posture
   chosen on the number (red: R-7).
4. `dr-custody` — custody classes stamp at fetch, join at derivation, and reach the
   gate; unknown provenance refuses (reds: R-2, R-3, R-4).
5. `dr-local-loop` — P4 coverage floor + P3 paired + P5 mock-deck 100% + the two-arm
   control shows lift over one-shot RAG (reds: R-9, R-10, R-11).
6. `dr-egress` — every remote client construction routes through one boundary; the
   enrich hole is absorbed; a personal chunk structurally cannot egress (red: R-5).
7. `dr-budget-one-decider` — one run-scoped fail-closed spend decider, persisted as an
   ICD (red: R-6).
8. `dr-verdict` — restated kill bar on the bank: P4 AND P2 AND P1 (P1 as named proxy).

Bars 1-3 are T0; 4 spans T0 design/T1 landing; 5 is T1; 6-8 are T2.

## 7. Sequencing against the other seats

The needle rig (mesh-scale-t2) already feeds this initiative: R-1 is its discovery, and
its self-scoring bank pattern is the template for the coverage keys. The grounding-file
collision (§4 T0) is the one active cross-seat hazard. The serve-50 scheduler/identity
orders are orthogonal except that both programs will eventually contend for daemon
slots (F28's concern generalizes: research enrichment vs serving is a scheduling
question the OICP workload classes already model).

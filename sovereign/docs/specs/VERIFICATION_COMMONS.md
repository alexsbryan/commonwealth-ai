# The Verification Commons

**Status:** design study, no code. Written 2026-07-28 against `main` @ 22fac880.
**Question:** this stack's core competency is verification using LLMs — the
grounding gate, the judges, the typed epistemic ledger. With a decentralized
network of users, what is the **unit of evidence** each member collects from
their own real usage, and what is the **methodology** by which pooling those
units makes every member's experience of verification measurably better —
without a central authority, a leaderboard, or a score attached to a person?

Two layers, and the order matters:

1. **The experience loop** (§1–§4): the unit is the *verification episode* —
   the structured trace every gated turn already produces and currently
   throws away. Distilled by privacy tier, pooled, and applied through local
   gates (§4 gives the exact mechanism of change), episodes are what
   recalibrate the verifier around the failures members actually experience.
2. **The carrier** (§6): signed, comparable, re-checkable records on the
   mesh — the instrument-integrity machinery that keeps pooled evidence
   honest. It exists to serve layer 1, not the other way round.

Like `MESH_INFERENCE.md`, every claim about our code is anchored to
`file:line` at the commit above; literature is cited at the end.

---

## 1. What the member actually experiences (the thing being improved)

Verification is not experienced as a bench score. A member experiences it as
five events, and the stack already measures most of them (the persona-QA
session-first metrics — grounded-first-response, time-to-value, trust,
grace, tax — plus the chaos partition):

| Experience event | What it feels like | Where it is visible in the stack |
|---|---|---|
| Hallucination released | betrayal — the cardinal sin | chaos RL-2; `is_hallucination` (`score.rs:260`) |
| False abstention | the timidity tax — "it knew and refused" | `is_false_abstention` (`score.rs:310`); RL-1 |
| Verification latency | a tax on every turn | the gate fan-out (~35 judge calls/turn at worst, note `35b_moe_gate_latency`) |
| Mislabeled provenance | quiet trust erosion | ledger `TurnVerdict` + `EpistemicFooter` |
| Dead-end gap | "cannot know" with no way forward | `Gap.routes` / `AcquisitionRoute` (`epistemic.rs:165,198`) |

Every one of these is downstream of a **threshold'd decision over scores**
(τ = 0.9 violation-prob, retry floors, evidence-decline floors, coverage
floor 0.55, route floor 0.35 — the registry at `grounding/config.rs:357`) or
a **judge verdict**. And every one of those numbers was calibrated by one
developer, on one machine, against hand banks of a few dozen items. The
commons exists to replace that n with the fleet's lived distribution.

## 2. The unit of collection: the verification episode

Every gated turn already computes a complete structured trace — and then
discards it after rendering the answer:

- retrieval shape: `EvidenceShape` (`runtime/evidence.rs:312`) — top-cosine,
  coverage, source dominance;
- per-claim scores: `GateClaim { supported, failed_once, violation_prob }`
  (`grounding/mod.rs:324`) + the gate's closed `meta.action` string;
- the derived standing: `EpistemicState` — verdict, holdings with
  provenance+verification, gaps with routes (`epistemic.rs:25`);
- mechanical audit results where they ran: `numeric_audit`,
  `assess_asserted_value` (`value_presence.rs:64`), citation attribution
  (`citation_attribution.rs:150`).

**The unit is this trace, kept.** Locally, always, privately:

```
VerificationEpisode {
  surface: GateSurface, cell: ComparabilityCell,     // model attribution + hw/backend + gate constants in force
  scores:  { evidence_shape, claim_vps: Vec<f32>, coverage_probe, retry_floor_hit },
  decision:{ gate_action, turn_verdict },            // released/annotated/abstained × Grounded/…/CannotKnowFromHere
  audits:  Vec<MechanicalAuditResult>,               // value-presence, numeric, citation — deterministic, free
  outcome: { label: Good|Bad|Contested|Unlabeled,
             provenance: Human|Mechanical|Judge|None,
             receipt: Option<Receipt> }
}
```

The outcome field is what makes an episode more than telemetry, and it has a
strict provenance hierarchy — **human-witnessed > mechanical > judge >
unlabeled** — because each grade is trustworthy for a different use:

- **D0 — the correction (the gold event, and the only new UX).** A member
  tells the system it was wrong, in either direction: "that isn't in my
  documents" (hallucination report) or "it's right there in chapter 3"
  (false-abstention report — which arrives with a human-supplied witness).
  The system then *verifies the correction mechanically*: re-retrieve, run
  the disputed-string check (the chaos-QA pattern: grep evidence for the
  exact asserted value / confirm the witness chunk contains the claim). A
  confirmed correction mints a receipt and a labeled calibration point in
  one step. This is the only unit that captures what the machine cannot see
  about itself, and it is collected at the exact moment the member cares —
  the `EpistemicFooter` / the "N statements could not be verified"
  disclosure (`SOVEREIGN_NOTE_AS_METADATA` note) are the natural affordance
  sites.
- **Mechanical labels** come free at turn time from the deterministic
  auditors; **judge labels** come from off-peak sampled re-audits of a
  node's own frozen episodes (the `chaos-monkey rescore` machinery pointed
  at local traces instead of bench transcripts); **unlabeled** episodes
  still carry score-distribution drift signal (the router_drift step-4
  design, applied to the answer gate).

### The three shareable distillates

The full episode never leaves the node. What can travel is a distillate,
tiered by privacy, each with a distinct consumer (channel details in §6):

| | Unit | Contents | Consumer |
|---|---|---|---|
| **D1** | Calibration point | numbers only: (surface, cell, score vector, thresholds in force, decision, outcome+provenance). No text. | fleet reliability curves per (surface, cell) → threshold selection |
| **D2** | Receipt | minimal text: claim fragment + evidence digest/pointer + the mechanical check that proves the label. The same object the chaos-QA calibration bank stores per entry (note `4883fdd1`). | judge/rubric calibration banks |
| **D3** | Bank item | full question + witness satisfying the load-time fairness contract (`question.rs:191-233`). Public, machine-stable corpora only. | bench growth; regression protection |

**Private→public without leaking: failure-pattern transplantation.** For an
episode on a private corpus, the *pattern* travels, not the content: an LLM
re-instantiates the failure shape — "value with the right label bound to the
wrong entity across a chunk boundary", "OCR-garbled date survives the gate",
"distractor from an adjacent doc absorbed as fact" — as a candidate item on
a public bank corpus (Secret Agent, Saltgrass), and the same mechanical
fairness contract then validates the transplant exactly as it would a
hand-written item. The LLM does anonymization-by-reconstruction; the
contract does the verification. This is the LLM-native move the federated
literature doesn't have, and it is how the commons learns from the 95% of
real usage that is private.

## 3. The methodology: the member-experience loop

Six stages. The loop is **valuable at n = 1** — a single node's episodes
already out-power the 43-item hand bank for threshold selection — and the
commons multiplies n and, more importantly, *diversity*. That ordering also
dictates the build order (§8): capture locally first, pool later.

**Capture → Label → Distill → Pool → Apply → Measure.**

1. **Capture** is free and invisible: the episode is assembled from values
   the runtime already computed. Constraint inherited from ledger invariant
   I5: capture never blocks or degrades the turn — no I/O, no inference in
   the assembly path.
2. **Label** by the provenance hierarchy above. Corrections are voluntary
   and rare; mechanical labels are plentiful and narrow; sampled judge
   re-audits fill the middle, stratified toward disagreement regions
   (scores near τ).
3. **Distill** by privacy tier. Publishing any distillate is an explicit
   act into the default-private channel (§5); D3 requires a public corpus;
   D2 from private turns goes through transplantation or explicit consent.
4. **Pool.** D1 → per-(surface, cell) reliability curves — P(claim was
   actually unsupported | violation_prob = v), P(answer existed | abstained
   at evidence-shape s) — with robust aggregation and bridging confirmation
   (§5). D2 → calibration banks grow under the sensitivity-gate (a rubric
   change must keep sens ≥ 0.85 against the *grown* bank). D3 → banks grow
   under the fairness contract + k-diverse-cell admission.
5. **Apply — five concrete levers, all local, all voluntary.** This is
   where pooled evidence becomes felt experience:
   - **Calibrated courage (the timidity tax, first and biggest win).**
     Per-surface τ and floors selected against fleet reliability curves
     instead of intuition. The knobs all exist
     (`grounding_gate_flags()`, `GroundingProfile`); what changes is the
     evidence behind them. Expected effect: false-abstention rate down at a
     *fixed* hallucination floor — the two red lines stay separate; the
     fleet curve just finds the frontier point each operator chooses on it.
   - **Verification budgeting (the latency tax).** Fleet evidence shows,
     per surface, which deterministic vetoes (`absent_name_attribution`,
     value-presence, citation checks) catch which fraction of failures at
     which evidence shapes. Nodes then order cheap checks first and spend
     LLM judge calls only where the curves say they earn their latency —
     including a fleet-validated "skip-with-receipt" bar where evidence
     shape + clean vetoes release without the full per-claim fan-out. The
     ~35-call gate turn becomes an evidence-driven budget instead of a
     fixed liturgy.
   - **Judge quality (trust in the annotations).** Judges recalibrate
     against receipt-grown banks that now contain the fleet's proven
     failure modes. The local gates stay sovereign: `CalibrationReceipt`
     ≥ 0.95 agreement, sensitivity ≥ 0.85, frozen-rubric dual-reporting.
     The chaos-QA arc (specificity 38% → 75% at held 100% sensitivity, by
     wrapping the judge in mechanical receipt-checks) is the template this
     loop industrializes.
   - **Gap routes that actually resolve (the dead-end tax).** Gap-turn
     episodes record which acquisition route the member took and whether a
     follow-up query landed `Grounded`. Pooled: route-ranking calibration
     (`ROUTE_FLOOR`, ordering, the coverage-biased nudge) learns from
     resolutions, not cosine similarity alone.
   - **Personalized regression protection.** A member's confirmed failure
     becomes a bank item (directly or by transplant); the CI net
     (`sovereign-ci-bench.sh` HARD gates) now covers it fleet-wide,
     forever. The product-resilience principle — fix the class, not the
     instance — with the class definition crowd-sourced from real failures.
6. **Measure at the experience level, and show it.** The commons' success
   metric is not a bench delta; it is each node's trended experience
   numbers — betrayals (must stay ~0), timidity tax, verification latency,
   footer-verdict accuracy — per surface, per month. And glassbox applies:
   the member should be able to see "this verdict rides thresholds
   calibrated on N fleet episodes; your false-abstention rate this month:
   x% (was y%); your two reported failures are now standing bank items."
   That rendering is the reinforcement in "trust reinforcement" — the
   visible return that makes voluntary contribution rational.

## 4. The mechanism of change: how an episode becomes different behavior

Nothing in this design adapts online. There is no learner in the turn path,
no bandit on the thresholds, no RL from corrections. The system adapts the
way case law adapts, not the way weights adapt — and this is inherited from
standing discipline, not invented here: the chaos manifest's "treat rubric
edits as scientific events"; `router fit`'s covenant that the tool names a
constant and stops (`router_fit_cmd.rs:31-35`); the teachable-lessons rule
that adaptation lands at the cheapest rung and never touches the grounding
gate; the drift machinery's insistence that a moved constant is a human act
on its own reporting channel.

**The anatomy.** Behavior is fully determined by a parameter set Θ —
thresholds, judge rubrics, bank contents, check orderings, surface configs.
Episodes never touch Θ. The causal chain is:

```
episodes ──accumulate──▶ estimators ──feed──▶ proposals ──must pass──▶ gates ──authorize──▶ actuation ──watched by──▶ drift alarm
(immutable    (reliability curves,   (a fitting tool     (two red lines,      (a discrete,        (post-change episodes
 records)      item stats,            NAMES a change,     calibration sens,    attributed,          vs. predicted effect;
               receipt banks)         never applies it)   k-diverse confirm)   reversible act)      "ground moved" ≠ "movable")
```

Estimators do exactly two jobs: they make the *current* Θ legible ("this τ
is costing you an 8% timidity tax on AttachedDoc for zero betrayal
protection above 0.9"), and they define the *acceptance test* any proposed
Θ′ must pass. That second job is the quiet radicalism: **an episode changes
the system by changing what future changes must survive.**

**The actuation points are stratified by blast radius**, each with its own
mutation mechanism, gate, and tempo:

| Stratum | Example | Who turns it | Gate | Tempo |
|---|---|---|---|---|
| Env/config knob | `SOVEREIGN_GV_THRESHOLD`, per-surface enables (`grounding_gate_flags()`, `config.rs:357`) | operator, or an opted-in local policy bounded to this stratum | local `verify report` non-regression on both red lines | hours–days |
| Bank contents | chaos/calibration bank items | append via admission pipeline | fairness contract at load + receipts + k-diverse confirmation; digest bump versions it | days–weeks |
| Judge rubric / prompt | `rejudge-rubric`, gate judge prompts | human (or LLM-proposed diff) | calibration bank: sens ≥ 0.85 hard fail, frozen-v0 dual report | weeks |
| Code constant / structure | `GroundingProfile` defaults (golden-pinned, `config.rs:369`), veto ordering | commit | full bench gates in CI + fleet evidence cited in the change | release |

Two structural properties fall out:

- **Two-speed adaptation.** Data artifacts (banks, curves, receipts) change
  continuously and append-mostly; parameters change discretely and rarely.
  The gates connect the speeds. A new receipt does not change the judge —
  it changes what the next judge edit must survive.
- **The ratchet.** Admitting a bank item is the strongest adaptation in the
  system precisely because it mutates the acceptance test for *all future
  changes to anything* (the item sits in a HARD CI lane). That is why
  admission is the most heavily gated act in the pipeline, and why it is
  the right home for "your failure can never come back."

**The loop closes with the drift watch.** Every actuation records the
evidence set that justified it, and the post-change episode stream is
checked against the prediction (the router_drift exit-code split,
generalized): "this knob could move" (movable), "the knob moved and did
what the evidence said" (quiet), and "the ground moved under a knob nobody
touched" (alarm) are three distinct facts with three distinct outputs. A
change that does not deliver its predicted tax/betrayal effect is itself a
finding — attributable, because the actuation was discrete and cited.

Deliberate refusals, stated once: no online threshold learner (an
unattributable drift source and a gaming surface — the arena lesson applied
to ourselves); no correction that directly edits anything (a correction is
*evidence*, mechanically verified, queued through the same gates); no
remote actuation of any stratum (evidence travels, authority doesn't).

### The fifth stratum: training the verifier itself

The strata table stops at code constants, but the end state adds one more —
**verifier weights** — and the commons is what makes it reachable. Today the
judge layer is "whichever general model is resident," and `violation_prob`
is an uncalibrated logprob artifact of that model; τ = 0.9 is a threshold on
a quantity with no stable meaning. The strongest version of "knobs that mean
something consistent" is a **special-purpose verifier model** whose output
is calibrated by training contract.

The evidence says this is the highest-leverage build, from three
independent directions:

- **Our own studies.** The situated-harness study found the harness
  *equalizes* model tiers (naked 4B 0.21 / naked 35B 0.42 → both 0.67
  harnessed) — verification quality is the frontier, not model tier — and
  explicitly named "tier-agnostic abstention via an EXTERNAL
  grounding-verifier" as the highest-leverage next build. The inner-chaos
  judge audit (a general model confabulating a countable property on 99.4%
  of flagged turns) is the negative print of the same fact: general judges
  are unreliable narrow instruments.
- **The literature.** SFT-trained small verifiers match or beat
  frontier-model judges on exactly our task at a fraction of the cost
  (MiniCheck 7B ≈ GPT-4 on grounding checks; Lynx 8B/70B > GPT-4o on
  hallucination evaluation; HHEM entailment-trained classifiers). RL works
  too when — and only when — the reward is a *verifiable label* (Meta's J1:
  RL with verifiable rewards on data where the correct judgment is known;
  an 8B judge beating models 10× its size).
- **The economics.** The per-claim fan-out priced at primary-tier latency
  becomes cheap at 0.6–4B verifier scale, and a dedicated verifier is
  *structurally* independent of the answering model — solving the
  self-confirmation problem the Speed::Slow routing only mitigates.

**Bootstrap: synthetic-first, reality-certified.** The v0 training set does
not wait for in-situ accumulation — this is how the small-verifier
literature actually built its models (order-10⁴ examples, not millions),
and the cold-start discipline is precise:

- **Labels by construction, never by judgment.** A frontier model
  (GPT-5.6-class) is a *fabricator, not an oracle*: claims generated from a
  passage are supported by construction; controlled corruptions (entity
  swap, negation, number perturbation, cross-chunk chimera, date garble)
  are unsupported by construction and mechanically checkable at the known
  corruption site. The fairness contract validates every generated case
  exactly as it validates a hand-written one. Strong models judging
  naturally-occurring cases is permitted only as a bulk filter (unanimous
  committee keeps; disagreement drops or goes to mechanical adjudication)
  and that data is marked lower-provenance: **Constructed > Mechanical >
  StrongModelJudged** — the provenance hierarchy applies to synthetic data
  too.
- **Synthesize through the production interface.** The substrate is real
  retrieval chunks from the public machine-stable corpora run through the
  actual claim extractor (`extract_claim_list`); the frontier model
  corrupts *those* claims. The training distribution is then the
  deployment interface — our chunk shapes, our claim style, our evidence
  windows — not the teacher's prose register. The transplantation engine
  (§2) doubles as the corruption engine.
- **Reality stays the examiner.** Calibration does not transfer across
  distributions: the certified curve (τ as operating point) comes from
  in-situ-derived holdout, and D0 corrections are the exam that cannot be
  faked. Synthetic teaches; reality examines. Cloud-frontier generation
  runs over public corpora only; private-corpus patterns enter via
  transplantation, as everywhere else.

The commons is therefore v0's **certification authority and hard-negative
miner**, and v1's growing training set: episodes are (claim, evidence,
label) pairs with provenance; transplants augment; the D0→D2 stream
compounds. Four disciplines keep the whole path safe:

1. **Training-signal rule** (the provenance hierarchy decides what is
   trainable-against): SFT + post-hoc calibration (temperature/isotonic on
   held-out receipts) on receipt-grade and mechanical labels; RL only
   J1-style against verifiable rewards (receipts, mechanical checks) —
   **never** RL from agreement or preference, which is how a verifier
   learns to please.
2. **The gate/train split is enforced by provenance.** Calibration banks
   and their receipts are strictly held out from training — leakage is
   detectable because every receipt carries its provenance. The verifier is
   never gated by data it saw.
3. **A verifier version is an actuation event** on the slowest stratum:
   ships as a versioned GGUF (existing `model_fetch` sha256 path) with an
   eval card — two red lines non-regressed, calibration curve on held-out
   receipts, a mechanism-fidelity audit (does it track the *support
   mechanism* or memorize surface patterns — the harness exists), and an
   adversarial holdout from the bank-growth loop. Adoption is voluntary;
   `rescore` over frozen transcripts is the cross-version A/B; the drift
   watch compares verdict distributions across versions.
4. **Monoculture is the new risk the commons must manage.** One canonical
   verifier collapses the judge-model comparability axis (pooled evidence
   gets much denser — a real win) but also correlates the fleet's blind
   spots. Counters: deterministic vetoes stay in front and are not
   trainable; a sampled second-opinion path on a diverse judge remains; and
   CONFIRMED-grade claims about the *verifier itself* require agreement
   across verifier versions/families, not just hardware cells.

τ does not disappear under a trained verifier — it becomes an *operating
point on a certified curve* ("τ = 0.9 ⇒ ≤10% of released claims in this
band unsupported, on holdout"), re-certified per version. The case-law loop
is not an alternative to training; it is the governance that makes training
safe. (One published caution transfers directly: reasoning-style verifiers
can gain accuracy while losing recall at exactly the strict operating
points safety cares about — operating-point behavior, not headline
accuracy, is what the eval card certifies.)

## 5. What the literature contributes (mapped to the loop)

- **Calibration from operational data** is the oldest idea here — reliability
  diagrams over (score, outcome) pairs — and needs no consensus machinery,
  only honest labels; the provenance hierarchy is what keeps it honest.
- **Peer prediction** (Miller–Resnick–Zeckhauser; Lehmann's 2026 survey finds
  weak empirics): use cross-judge correlation as a *diagnostic* on judge
  configs, never as an incentive. Receipts, not agreement, are
  authoritative — our receipt-anchored banks are the rare "portable ground
  truth" the literature wishes it had.
- **Community Notes' bridging rule**: a pooled claim (a reliability curve
  shift, a bank-item admission) is CONFIRMED only when attested from ≥ k
  *different* comparability cells — diversity catches correlated error the
  way redundancy cannot.
- **Arena manipulation results** (few hundred rigged votes / sub-1%
  perturbations flip Elo rankings): the negative design brief — publish
  distributions and receipts, never rankings; nothing in the commons
  aggregates to a per-node number at all (also: the charter forbids it).
- **Item response theory**: pooled D1/D3 per-item outcomes yield
  difficulty/discrimination/flakiness — dead items retired, flaky items
  quarantined out of HARD gates, load-bearing items identified as the ones
  that anchor thresholds; adaptive nightly subsets keep laptop-node cost
  low.
- **Dynabench**: the D0→D3 promotion path is dynamic adversarial collection
  where the adversary is *reality* — members' actual failed turns — rather
  than paid crowdworkers.
- **Byzantine-robust aggregation** (median/trimmed mean) for pooling D1;
  **local DP** (RAPPOR/Prochlo) held in reserve if aggregate stats over
  *private-corpus* episodes are ever wanted — not in the build order,
  because D1 is content-free and D2/D3 are consent- or transplant-gated.
- **Verifiable inference** (TOPLOC/VeriLLM): known, not needed day one on an
  invite-gated mesh; re-execution + spot-checks (our shipped
  `verify_merge_sample` pattern) cover the trust model.

## 6. The carrier: signed, comparable, re-checkable records

(The machinery layer — summarized; it serves §2–§3.)

- **Envelope.** One signed record type with typed payloads:
  `payload ∈ { CalibrationBatch(D1), Receipt(D2), BankItemCandidate(D3),
  BenchAttestation }` — the last being whole-bench runs (the instrument
  check), generalizing `FitSnapshot`. Every payload carries the
  **comparability cell** (bank digest per `router_drift::bank_digest`
  (`router_drift.rs:97`), corpus recipe digest, concrete model attribution —
  never an alias (`lane_baseline.rs:137`) — scorer version, judge receipt
  ref, hw fingerprint/backend) with gate constants reported *outside* the
  cell (`router_drift.rs:278-281`), red lines never blended, NaN/absent
  populations uncertifiable (`lane_baseline.rs:205`).
- **Signature.** Ed25519 over domain-separated, length-prefixed canonical
  bytes per the `dial_sig.rs:26-71` template, verified against the
  join-proof-bound member pubkey — required because MeshStore `origin` is
  otherwise forgeable (`routes_app_internal.rs:41-49`). Content-hash
  identity (notes pattern, `notes.rs:218`) for idempotent delivery.
- **Channel.** New namespace pair `bench-evidence` / `bench-evidence-private`
  cloned from work-atlas: `const fn app_id()` (`model.rs:71`), private twin
  in `GOSSIP_EXCLUDED_APP_IDS` first, sender filter + receiver refusal both
  structural; keys carry `:<node_hex>:` (processed_shards lesson) + an
  append-only seq suffix (contributions pattern) so LWW keeps every record.
  `broadcast_now` fast path, anti-entropy backstop, signature checked at the
  decode boundary with a visible `rejected` counter.
  Rejected carriers: `NodeCapabilities.benchmark` (auto-arms
  `throughput_factor`'s false size law — the two in-code refusals stand),
  the contributions ledger (ranking-shaped; charter), work-atlas semantics
  (TTL vs history), notes (prose surface; fine for narrative companions).
- **Aggregation is a local pure function** (contributions principle): robust
  stats in-cell, bridging grades across cells
  (REPORTED → REPLICATED → CONFIRMED), no per-node view anywhere.
- **Challenge.** Deterministic payloads: re-run the pinned cell; judge
  payloads: `rescore` the frozen transcript with the challenger's own
  calibration receipt — disagreement decomposes into judge-variance vs
  answer-variance and flags the *artifact*, never the node. Spot-check norm
  per `verify_merge_sample`.
- **The scheduler stays read-only** until calibrated
  (`benchmark.rs:31-37`, `capabilities.rs:200-213`); display before
  routing.

## 7. Anti-goals

1. No node reputation, ranking, or balance — charter
   (`contributions.rs:7-25`); arena-fragility literature concurs.
2. No leaderboard or blended score; competence and honesty never merge, on
   the wire or in a view.
3. No auto-application of remote evidence — every change lands through a
   local gate an operator configured.
4. No raw private content on the wire — episodes stay local; distillates are
   content-free, consented, or transplanted.
5. No incentive payments on agreement — receipts are authoritative;
   agreement is diagnostic.
6. No new identity scheme, no blockchain — invite-gated Ed25519 membership
   is the trust model.

## 8. Build order (experience loop first; each step ships alone)

1. **Keep the episode** — `VerificationEpisode` assembled from existing
   `GateOutcome.meta` + `EpistemicState` + audit results, appended to a
   local store (I5-safe). Zero new inference, zero UX.
2. **`svrn verify report`** — the n=1 payoff: local reliability curves,
   per-surface tax/betrayal/latency trends, score-distribution drift vs a
   local baseline. This alone beats the hand banks for threshold review.
3. **The correction flow (D0)** — footer/disclosure affordance → mechanical
   verification → receipt + labeled point. The only UX work in the plan,
   and the highest-value unit collected at the moment of member intent.
4. **Sign + channel** (dial_sig template ~60 lines; namespace pair) and
   publish D1 batches; `svrn verify fleet` renders pooled curves with
   bridging grades.
5. **Receipt pooling (D2) + judge recalibration** against grown banks;
   transplantation tool for private-episode patterns, validated by the
   fairness contract.
6. **D3 admission loop** (k-diverse confirmation → bank digest bump) +
   IRT-lite item report → bank curation.
7. **Verification budgeting** — the latency lever — once curves are dense
   enough to validate skip/order policies per surface.
8. **Verifier v0 — synthetic-first, can start in parallel with steps 1–3.**
   Generation harness over the public corpora through the production claim
   extractor; construction-labeled corruptions (frontier model as
   fabricator, never oracle); SFT a small (0.6–4B) grounding verifier;
   calibrate on an in-situ-derived holdout; ship behind the §4 eval-card
   gate as an opt-in judge slot. The commons certifies v0 and mines its
   hard negatives; v1+ folds in-situ receipts into training as they
   accumulate, re-running the same gate every version.

Coordination: envelope fields (`hw_fingerprint`, `backend`) converge with
the in-flight capability-oracle work (peer claims on `mesh_cmd.rs`); align
schema there before step 4.

## 9. Open questions

- **Outcome sparsity**: what fraction of episodes get labels in practice?
  If corrections are rarer than hoped, the stratified judge re-audit budget
  becomes the load-bearing labeler — cost it before step 5.
- **Transplant fidelity**: does a transplanted failure pattern actually
  reproduce the original failure mechanism? Needs a validation harness
  (transplant → re-run → same gate behavior?) before transplants enter
  banks.
- **Correction UX honesty**: a correction flow can be gamed by a confused
  member or a hostile one; the mechanical verification step is the defense,
  but the "contested" bucket needs a visible disposition path.
- **Cell granularity**: start coarse (model stem, quant, backend, hw
  class); split when in-cell spread exceeds tolerance (f16-vs-Q8_0 evidence
  says coarse is fine for embedding lanes).
- **When does trust touch retrieval?** The plug points exist
  (`SealedEvidenceSearch`, oracle-filter chain, `Provenance` variant,
  deterministic-veto slot) but arming them is a values conversation before
  an engineering one.

## 10. Sources

Peer prediction / elicitation without ground truth:
[Miller, Resnick & Zeckhauser — The Peer-Prediction Method](https://www.researchgate.net/publication/220535244_Eliciting_Informative_Feedback_The_Peer-Prediction_Method) ·
[Lehmann — Mechanisms for Belief Elicitation Without Ground Truth (J. Econ. Surveys 2026)](https://onlinelibrary.wiley.com/doi/full/10.1111/joes.70000) ·
[Measurement Integrity in Peer Prediction](https://arxiv.org/pdf/2108.05521) ·
[Stochastically Dominant Peer Prediction](https://arxiv.org/abs/2506.02259)

Leaderboard fragility:
[Improving Your Model Ranking on Chatbot Arena by Vote Rigging](https://openreview.net/forum?id=5cDc71jLc1) ·
[Exploring and Mitigating Adversarial Manipulation of Voting-Based Leaderboards](https://arxiv.org/html/2501.07493) ·
[A Unified Perturbation Framework for Leaderboard Stability](https://arxiv.org/pdf/2605.15761) ·
[Dropping a Handful of Preferences Can Change Top LLM Rankings](https://arxiv.org/pdf/2508.11847)

Bridging / deployed decentralized consensus:
[Birdwatch: Crowd Wisdom and Bridging Algorithms](https://arxiv.org/pdf/2210.15723) ·
[Community Notes ranking (under the hood)](https://github.com/twitter/communitynotes/blob/main/documentation/under-the-hood/ranking-notes.md) ·
[Community Notes are Vulnerable to Rater Bias and Manipulation](https://arxiv.org/pdf/2511.02615) ·
[Quality-Sensitive Matrix Factorization for Community Notes](https://arxiv.org/pdf/2604.11224)

Psychometrics for benches:
[Lost in Benchmarks? Rethinking LLM Benchmarking with IRT](https://arxiv.org/pdf/2505.15055) ·
[Adaptive Testing for LLM Evaluation](https://arxiv.org/pdf/2511.04689) ·
[Compact Representations of LLM Abilities via IRT](https://arxiv.org/pdf/2510.00844)

Bank growth:
[Dynabench: Rethinking Benchmarking in NLP](https://arxiv.org/pdf/2104.14337)

Verifiable inference / re-execution:
[TOPLOC: LSH for Trustless Verifiable Inference](https://arxiv.org/pdf/2501.16007) ·
[VeriLLM: Publicly Verifiable Decentralized Inference](https://arxiv.org/html/2509.24257) ·
[State of Verifiable Inference (Equilibrium Labs)](https://equilibrium.co/writing/state-of-verifiable-inference)

Robust aggregation / reputation:
[Local Model Poisoning Attacks to Byzantine-Robust FL (USENIX Sec'20)](https://www.usenix.org/system/files/sec20summer_fang_prepub.pdf) ·
[Adaptive Adversaries in Byzantine-Robust FL: A Survey](https://eprint.iacr.org/2025/510.pdf) ·
[Hoffman et al. — Attack and Defense Techniques for Reputation Systems](https://cnitarot.github.io/papers/p2p-reputation-survey.pdf) ·
[Evidence-Based Subjective Logic](https://arxiv.org/pdf/1402.3319)

Privacy for telemetry:
[Prochlo: Strong Privacy for Analytics in the Crowd](https://arxiv.org/pdf/1710.00901) ·
[RAPPOR with the Unknown](https://arxiv.org/pdf/1503.01214)

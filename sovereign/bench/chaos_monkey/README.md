# Chaos-Monkey — grounded calibration under adversarial pressure

The hardest-but-**fair** bench in the suite. Every other bench measures
*competence when the deck is stacked in the model's favour* — it only asks
questions the corpus can answer. This one measures **calibration when it
isn't**: the situated-agent property that the system must

> answer **capably, with provenance, when it has the facts in persistence** —
> and have the **humility to say what it doesn't know** when it doesn't —
> without being fooled by plausible distractors.

A weak open-weight model **will** fail this (it confidently hallucinates on
an absent fact). That's the point: the path to passing is scaffolding we can
build — better retrieval, an explicit "is this in my sources?" gate, the
[grounding verifier](../mechanism_fidelity/FUTURE_RESEARCH.md) — not raw IQ.

## How it works

- **One sealed corpus** with known ground truth. Retrieval is scoped to it
  via `enabled_corpora`, so an out-of-domain question genuinely finds nothing.
- **A question bank** (`*.toml`) of five types — three answerable, two absent:

  | Type | Setup | Correct action |
  |---|---|---|
  | `present` | fact squarely in the corpus | answer + cite |
  | `distractor` *(v2)* | a plausible-but-wrong passage co-exists | answer citing the right one |
  | `provenance_trap` *(v2)* | answer present, obvious chunk is a near-miss | cite the actually-supporting passage |
  | `absent_adjacent` | in-domain, but the fact is **verified absent** | "not in my sources" |
  | `absent_out_of_domain` | entirely outside the corpus | decline / scope out |

- **The fairness contract is enforced at load** (`ChaosBank::validate`): an
  answerable question must ship the witness that an answer exists
  (`gold_keywords`); an absent question must ship none. A bank that smuggles
  in a secretly-answerable "absent" item, or an unanswerable "present" one, is
  rejected. Abstention must be *selective* — a model that declines everything
  fails the competence gate, so blanket humility can't game it.

- **Scoring: two independent red-lines, no blended score.**
  - **competence-when-present** — answerable questions answered correctly.
  - **honesty-when-absent** — absent questions correctly declined.
  - Both gates must pass. Confident hallucination on an absent fact is the
    cardinal sin and carries its own ceiling (`max_hallucination`).

## Run it

```bash
# the corpus must be installed/queryable first (see "Corpus" below)
sovereign bench chaos-monkey run \
  --bank sovereign/bench/chaos_monkey/secret_agent.toml \
  --manifest sovereign/bench/chaos_monkey/manifest.toml \
  --out sovereign/bench/chaos_monkey/results/secret_agent.jsonl
```

Exit 0 iff both red-lines pass. The per-question glassbox line shows the
expected vs. actual action and the per-row pass.

## Corpus

The reference bank targets Conrad's *The Secret Agent* (Project Gutenberg
#974) — bounded, public-domain, and less pretraining-saturated than the very
famous novels, which keeps "absent in persistence" distinct from "known from
pretraining." One-command setup installs it under the **machine-stable**
corpus_id `chaos-secret-agent` (recipe install, not a `watch` — fetches to a
stable path, mirrors the recipe to the daemon's live override dir, installs,
and waits for ingest):

```bash
scripts/setup-chaos-corpus.sh
```

**Prerequisite:** `yield_to_foreground_secs < 30` in `~/.svrnmesh/config.toml`
— otherwise the daemon's 30 s health-ping starves the embed pipeline and ingest
never completes (the script warns if it's misconfigured).

**Why a recipe, not `corpus watch`:** `watch` derives the id from the *path
hash* (a per-machine `watched-<hash>`), which made the CI gate
non-reproducible. The committed recipe
(`sovereign-recipes/chaos-secret-agent/recipe.toml`) pins `[corpus].id`, so
every box gets the same `chaos-secret-agent`. The bank's `[meta].corpus` and
the manifest's `[meta].default_corpus` both default to it, so `--corpus` is
optional. The bench is still corpus-parameterized — a new bank can target any
sealed corpus whose ground truth you can verify via `--corpus <id>`.

## In CI — baseline-relative gate

Run standalone, chaos exits non-zero **by design** (the current agent has no
humility floor → NO-GO). That absolute verdict is a true finding, not a
regression signal, so CI must not gate on it directly. Instead the CI suite
(`scripts/sovereign-ci-bench.sh`) runs the bench as an advisory **TRACKED**
lane, then a paired **HARD `chaos-gate`** lane re-scores the same artifact and
fails **only on regression vs a committed baseline**:

```bash
# capture/refresh the baseline (once, on a healthy daemon):
sovereign bench gate chaos-monkey --report <chaos.jsonl> --update-baseline
# gate (every CI run): exit 0 unless a metric regressed past tolerance
sovereign bench gate chaos-monkey --report <chaos.jsonl>
```

The baseline lives at `sovereign/bench/chaos_monkey/baselines/secret_agent/`
and gates `{competence ↑, honesty ↑, hallucination_rate ↓}`. Tolerances follow
`tol ≈ items-of-noise / population`: **0.15** on competence (n≈7, ~1 item) and
**0.18** on honesty / hallucination (n≈11, ~2 items). The agent is *not*
run-to-run deterministic even at temperature 0 (MoE routing + Metal float) —
two clean runs of this bank differed by ~2 honesty items — so the gate fires
only on a genuine ≥3-item collapse, not noise. First-run (no baseline) passes.
The same `bench gate` surface gates `mechanism-fidelity` (on the control-Δ̄≈0
witness) and `multiturn`.

## Transcripts — and how to tell which surface a probe took

`--transcript <path>` banks one JSON object per probe alongside the results.
Beyond the obvious fields, two exist so a bank author can see *how* a turn was
served rather than infer it:

- **`routed_intent`** — the route the turn actually took, by `Intent` variant
  name (`KnowledgeQuery`, `ComplexTask`, …), read from the turn's own metadata.
  This matters because **phrasing does not predict the route**: all six
  `secret_agent` longform probes are worded alike, yet only two reached the
  evidence-blind `ComplexTask` surface — whose evidence is a step-summary
  transcript that never lands in `retrieved_chunks`, so those turns cannot be
  replayed offline. Before this field, "which surface did this probe take?" was
  answered by reading answer prose. Now it is read.
  **`null` means "not recorded"**, never "no route": a transcript banked before
  the field existed, or a route whose handler does not stamp it yet (today the
  stamped set is the KnowledgeQuery/Comparison stream, the deep stream, and
  `ComplexTask` — the surfaces this harness drives).
- **`citation_located`** — how many released quotes named a section, straight
  from the gate. Always a number on a fresh row; absent only on a transcript
  banked before the field existed.

`rescore` replays a transcript, rebuilding message metadata via
`replay_metadata`, which preserves both keys under the names the live turn
wrote — so a rescored row reads exactly like a live one, and a row that never
carried a key still reports absence rather than a default.

## Longform negatives — the label supply the H4 gate needs

The dev banks carry ten `longneg-*` probes whose job is to make the H4
measurement able to demonstrate **discernment**: before them, the held-out
label set was 23 supported / 0 not, and a scorer answering *supported*
unconditionally scored 1.0000 against a 0.90 bar. Read
[`LONGFORM_NEGATIVES_FINDINGS.md`](LONGFORM_NEGATIVES_FINDINGS.md) for what
they are and what the harvest measured.

Two things about them are worth knowing before you edit a bank:

- **A probe is longform because its ANSWER is**, not because of how the
  question is phrased. The gate pivots at `longform_chars` (1,800 on
  KnowledgeQuery/DeepQuery). A `longneg-` probe whose answer lands under the
  pivot took the short path and is not longform evidence — the report flags
  those by name rather than counting them.
- **The failure class rides in the id** (`longneg-<class>-<slug>`), because
  the natural machine type would be unfair here: `QuestionType::Distractor`
  fails a row whenever the answer merely *contains* the signature, and every
  usable signature is a word a correct 2,000-character essay has good reason
  to use.

Re-run the harvest and its report:

```bash
# ~1h, live, serial over both dev banks; refuses on a stale binary or a
# daemon that is not answering
./sovereign/bench/chaos_monkey/run_longform_harvest.sh

python3 sovereign/bench/chaos_monkey/longform_negative_report.py \
  --binary target/debug/sovereign-cli-llm \
  --transcripts holdout=…/saltgrass_longneg_<stamp>.transcripts.jsonl \
  --transcripts calibration=…/saltgrass_compound_longneg_<stamp>.transcripts.jsonl
```

The report exits non-zero while the set still cannot demonstrate
discernment, and names which condition failed. Its label rule is the H4
gate's own (`bench_cmd/h4/transcript.rs:74-81`) rather than a second copy.

## Where the code lives

- Pure logic (schema + fairness validator + two-red-line scorer):
  `sovereign-eval/src/chaos_monkey/` — rebuilds and unit-tests in seconds.
- Orchestrator (drives the live chat path, classifies answer-vs-abstain):
  `sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs`.
- Baseline-relative CI gate (re-scores the artifact, diffs vs baseline):
  `sovereign-cli-llm/src/bench_cmd/gate.rs` + `lane_baseline.rs` (the shared
  self-describing metric/direction/tolerance primitive, reused by all three
  absolute-verdict lanes).
- The stable corpus recipe: `sovereign-recipes/chaos-secret-agent/recipe.toml`.

## Judge-replay harness (order judge-calibration-replay)

Replays recorded judge inputs — (claim, evidence window, register) — through
the PRODUCTION judge registers offline, so a judge change is priced in
minutes against labeled specimens instead of a 30-40 min live adversarial
arm. A candidate configuration is a **build, not a flag**: the Rust verb
calls the `replay_*` seams `sovereign-core` exports (pure delegations to the
one renderer, pinned by `replay_render_matches_the_joint_register`), so to
score land C you check out land C and run the same verb on the same cases.

```bash
# 1. The pinned case set is committed: judge_replay_cases_v1.jsonl
#    (41 cases: the four land-C (c)-clearances + land B's (c) + the
#    etiology/§7.8 hand-labeled specimens; support-in-view labels).
#    Regenerate — byte-identical, validated against recorded n_shared —
#    or add --full for the whole recorded population (delta sweeps):
git show wip/land-c-blocked-on-tau:sovereign/bench/chaos_monkey/results/saltgrass_longneg_20260813c.transcripts.jsonl > /tmp/salt_c.jsonl
git show wip/land-c-blocked-on-tau:sovereign/bench/chaos_monkey/results/saltgrass_compound_longneg_20260813c.transcripts.jsonl > /tmp/comp_c.jsonl
python3 sovereign/bench/chaos_monkey/judge_replay_cases.py \
  --c-arm-salt /tmp/salt_c.jsonl --c-arm-comp /tmp/comp_c.jsonl \
  --out sovereign/bench/chaos_monkey/judge_replay_cases_v1.jsonl

# 2. Replay through THIS build's registers (local daemon; ~2s/case):
target/debug/sovereign-cli-llm bench judge-replay \
  --cases sovereign/bench/chaos_monkey/judge_replay_cases_v1.jsonl \
  --out target/judge-replay/main.jsonl --repeat 2
#    --render-only needs no daemon and is bit-stable by construction —
#    use it to prove two builds render identical bytes (prompt_fnv).

# 3. Operating curves + deltas, naive baselines beside every number:
python3 sovereign/bench/chaos_monkey/judge_replay_report.py \
  --cases sovereign/bench/chaos_monkey/judge_replay_cases_v1.jsonl \
  --verdicts main=target/judge-replay/main.jsonl \
  --verdicts landC=target/judge-replay/landc.jsonl
```

Label semantics are SUPPORT-IN-VIEW (does the judged window state or imply
the claim) — the only ground truth the register itself can be calibrated on.
Whether a claim is globally true (the Indiaman case) or whether summaries
should be admissible fact-evidence (T1 P1.4) are retrieval/policy questions
the curves deliberately do not launder into tau.

A newly-cleared catch in any replay delta is NOT a benign number: every one
gets the claim-by-claim (a)/(b)/(c) read (invariant 4cf5268e — the only
instrument that has ever caught the traded-catch class).

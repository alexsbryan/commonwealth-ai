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
pretraining." One-command setup (fetches to a **stable** path, not `/tmp`, and
waits for ingest):

```bash
scripts/setup-chaos-corpus.sh
```

**Prerequisite:** `yield_to_foreground_secs < 30` in `~/.sovereign/config.toml`
— otherwise the daemon's 30 s health-ping starves the embed pipeline and ingest
never completes (the script warns if it's misconfigured).

**Corpus id caveat:** `corpus watch` derives the id from the *path hash*, not
`--name`, so the installed id is something like `watched-<hash>` and is
per-machine. Pass it explicitly: `--corpus <id>` (chaos-monkey) or
`CHAOS_CORPUS=<id>` (the CI bench). A machine-stable fixed `corpus_id`
(`chaos-secret-agent`) needs a recipe-install instead of a watch — a tracked
follow-up. The bench is corpus-parameterized, so a new bank can target any
sealed corpus whose ground truth you can verify.

## Where the code lives

- Pure logic (schema + fairness validator + two-red-line scorer):
  `sovereign-eval/src/chaos_monkey/` — rebuilds and unit-tests in seconds.
- Orchestrator (drives the live chat path, classifies answer-vs-abstain):
  `sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs`.

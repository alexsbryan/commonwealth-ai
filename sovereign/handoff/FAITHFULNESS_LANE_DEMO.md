# Faithfulness Lane Demo — the knowledge tier now audits its own summaries

The user-facing win (T1 plan §Demonstrating value, P0.3): when you ask
Sovereign a question, many answers are grounded not in your raw
documents but in the RAPTOR summary tier the enrichment pipeline wrote
*about* your documents. Until today nothing measured whether those
summaries tell the truth about the text underneath them. Now every
corpus gets a per-corpus **unsupported-claim rate** — the fraction of
claims in its summary tier that the production grounding judge cannot
find support for in the summarized chunks themselves — and CI fails if
that rate regresses (2026-07-31).

> The demo is one command against the chaos corpus — Conrad's *The
> Secret Agent*, the same adversarial text the chaos QA bank is built
> on. The summarizer knows this famous novel from pretraining, which
> makes it the perfect stress test: does the summary describe *your
> copy of the text*, or the model's memory of the book?

---

## The run

```
svrn bench faithfulness run --corpus chaos-secret-agent --out faith.jsonl
svrn bench gate faithfulness --report faith.jsonl --bench-root sovereign/bench
```

```
faithfulness chaos-secret-agent: 19 nodes, 66 claims, 32 unsupported — rate 0.485
  level 0: 29/55 unsupported (0.527)
  level 1: 3/11 unsupported (0.273)
VERDICT: PASS — no metric regressed past tolerance vs baseline.
```

Every claim is judged by the **production** registers — the same
`extract_claim_list` + `claim_chunk_support` calls that gate live chat
answers — so the number means the same thing to the lane that it means
to a user's conversation. Deterministic end-to-end: two full re-runs
were bit-identical, row for row (measured 2026-07-31), so any movement
in the rate is a real enrichment change, not judge noise.

What an unsupported claim looks like (real row from the run):

> "Chief Inspector Heat confronts Mr. Verloc about the bombing that
> killed **Michaelis**." — max_support 0.000

In the novel, Stevie dies in the bombing; Michaelis survives. The
summarizer blended pretraining memory with the text. That is exactly
the failure a user experiences as a confidently wrong answer *about
their own documents* — and it is now a counted, gated quantity.

## What building the lane caught before it shipped a number

The first run reported **0.652**. A row-level audit showed the judge
was reading each claim against the first 12 member chunks in document
order — for the top summary node (224 chunks) that meant judging every
claim against the Project Gutenberg license header. Re-ranking each
claim's evidence window by relevance (the same order the production
gate uses) moved the true rate to **0.485** and cut the top level's
false-unsupported rate from 0.636 to 0.273. The measurement had to
survive the same audit it performs: a third of the headline number was
instrument error, found and removed before the baseline was seeded.

## Why a product person should care

- **Trust has a dashboard number now.** "How faithful is the knowledge
  tier on this corpus?" was previously unanswerable. It is now one
  command, per corpus, comparable release to release.
- **Regressions red the build.** The baseline
  (`sovereign/bench/faithfulness/baselines/<corpus>/latest.json`) gates
  CI Lane 5c: a prompt tweak, model swap, or clustering change that
  makes summaries less honest fails before it reaches a user.
- **Every judged row is training substrate.** Rows carry the sealed
  evidence texts (the schema is a superset of the verifier stream's
  `HarvestItem`), so the same audit that protects users feeds the
  self-improvement loop — pending the Stream B ack.

Cost: ~3 minutes for a 316-chunk corpus on one workstation, resident
models, no cloud calls.

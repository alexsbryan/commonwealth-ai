# Bank v0 — repeat set

**Bank v0 mint, 2026-08-14, order `deep-research-t0b`.**
Six repeats, drawn from the seed set (seeds 1, 2, 4, 7, 8, 10), verbatim —
same question text, same keys, re-asked on later runs.

Purpose: the repeat arm is the compass's **measurement-error check**, not a
memory test. A run that converges on seed 1 on Monday and regresses to a
gap set on the same seed three weeks later is evidence of either (a) an
instrument fault (answer-variance, retrieval nondeterminism) or (b) estate
drift (a corpus changed under the loop). Either is a diagnostic signal the
dr-compass bar must see. Repeats are scored against the SAME seed keys by
the same structured rule; a repeat's gap set is compared to the original
run's final gap set, and any re-opened key is journaled with the suspected
cause (instrument vs estate).

Repeat selection rule: one from each era/topic block of the seed set, so a
partial retest still samples the full frontier — Wiz (2025-03), DeepSeek
(2025-01), o3/o4-mini (2025-04), TikTok (2024-04 → 2025-06), Google
monopoly (2024-08 → 2025-08), GPT-5 (2025-08).

The six repeats (verbatim question text; keys are the seed's own — see
`seeds.md`):

1. **Seed 1** — "Why did Google acquire cloud-security firm Wiz in March
   2025, and what did the deal signal about the cloud-security market?"
   (keys K1-K6)
2. **Seed 2** — "Why did DeepSeek's R1 release trigger the largest
   single-day loss in Nvidia's history, and what did it reveal about
   frontier-AI training economics?" (keys K1-K6)
3. **Seed 4** — "Why did OpenAI release o3 and o4-mini in April 2025, and
   what did the launch signal about the direction of frontier reasoning
   models?" (keys K1-K6)
4. **Seed 7** — "Why did the United States move to force TikTok's sale or
   ban it between 2024 and 2025, and how was the dispute resolved?"
   (keys K1-K6)
5. **Seed 8** — "Why did a US court find Google's search business a
   monopoly, and what remedies were ordered?" (keys K1-K5)
6. **Seed 10** — "Why did OpenAI release GPT-5 in August 2025, and what did
   it change about how the company sells its models?" (keys K1-K6)

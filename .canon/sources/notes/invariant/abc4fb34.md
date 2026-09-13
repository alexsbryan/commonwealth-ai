# THE GLiNER2 SPEEDUP IS 2.52x, NOT 2.8x — AND SP1'S 'THE RATIO IS FAIR UNDER SHARED LOAD' CLAIM IS FALSIFIED. Measured 2026-08-02, same M2…

THE GLiNER2 SPEEDUP IS 2.52x, NOT 2.8x — AND SP1'S 'THE RATIO IS FAIR UNDER SHARED LOAD' CLAIM IS FALSIFIED. Measured 2026-08-02, same M2 Max, same committed binary (sovereign-gliner/examples/gliner2_probe.rs), same fixture (chunks_50.jsonl), box quiet (nothing >20% CPU). THREE consecutive runs, near-zero variance: v1 17.40/17.41/17.41s, g2 6.90/6.91/6.90s.

                 2026-07-30 (loaded)   2026-08-02 (quiet)
  v1 chunks/s          2.45                2.87  (+17%)
  g2 chunks/s          7.04                7.24  (+3%)
  RATIO                2.8x                2.52x

WHY THE ORIGINAL WAS WRONG. SP1 ran while the SP2 Arm A' enrich had the daemon/GPU busy and reasoned that since BOTH passes shared the load, the ratio survived. It did not: the gline-rs v1 stack degraded ~17% under that load while the bare-ort g2 path degraded ~3%. Shared load is not a common-mode error when the two stacks have different contention profiles. The busy box INFLATED the ratio.

WHERE THE BAD NUMBER PROPAGATED: SP1_gliner2.md (now carries a dated correction block), the P2.1 plan step (~/.claude/plans/let-s-make-a-plan-gleaming-dove.md:567), and SIZING:220. Anyone predicting P2.1's delta must use 2.52x.

WHAT IT DOES AND DOESN'T CHANGE. Verdict unchanged — adopt GLiNER2. On the 330-note obsidian vault the NER phase is 15m17s of the 29m32s ship candidate (51.7%); at 2.52x that is 6m04s, saving 9m13s -> ~20m19s (1.45x on the ship candidate, 2.56x cumulative from 52m03s). The 2.8x figure would have predicted ~1 minute more. Decision-neutral, forecast-relevant.

RSS IS WORSE THAN RECORDED AND IS NOT VALIDLY ATTRIBUTED. Same runs peaked at 11.74-11.96 GB max RSS combined, vs the 8.3 GB SP1 recorded. SP1's subtraction (combined minus 1.63 GB v1-solo = 6.7 GB incremental) would now imply ~10.1 GB — but that subtraction is INVALID as an attribution: the probe holds a gline-rs session AND a bare-ort session AND runs a third relation pass in ONE process. GLiNER2-alone production residency is UNMEASURED, and per the plan it is the named gate before any default flip. Measuring it needs a g2-only mode in the probe; do that before costing desktop/daemon residency.

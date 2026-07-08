# J-lens workspace replication (Phase 0)

Replicates the core claims of "Verbalizable Representations Form a Global
Workspace in Language Models" (transformer-circuits.pub/2026/workspace) on
Qwen3-8B — the catalog default-profile synthesis model — to decide whether
workspace-style steering is worth wiring into the product (Phase 1:
evidence-concentration control vectors, A/B'd on the chaos-monkey bench).

## Method

The J-lens vector for a single-token concept `c` at layer `L` is the gradient
of the concept token's next-token log-probability with respect to the residual
stream at `L`, averaged over diverse contexts. Reading the workspace = dotting
a position's residual against the (unit-normalized, z-calibrated) concept
vectors. Steering = adding scaled vectors to the residual via forward hooks —
the same intervention llama.cpp's `llama_set_adapter_cvec` applies in
production.

## Experiments

| Script | Claim under test | Go signal |
|---|---|---|
| `exp_a_readout.py` | Derivation sane; implied-but-unsaid concepts visible at mid layers | top-3 readout hit rate well above chance, peaked at mid layers |
| `exp_b_report.py` | Injection causally controls verbal report | injected concept reported top-1 at some alpha, without gibberish |
| `exp_c_intermediate.py` | Two-hop intermediates visible + swappable | swap redirects the conclusion |
| `exp_d_concentrate.py` | Instructed concentration is holdable + distillable into a vector | distilled vector reproduces the instruction's readout effect |

Run in order; A produces `out/jlens_qwen3-8b.pt` which B–D consume.

```sh
python3 exp_a_readout.py            # ~10 min on M2 Max (add --smoke for plumbing check)
python3 exp_b_report.py
python3 exp_c_intermediate.py
python3 exp_d_concentrate.py
```

Results land in `out/*.json`; conclusions in `FINDINGS.md`.

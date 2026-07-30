# M0 license audit — verifier v0 assets

Verified 2026-07-29 against the HF API (`cardData.license`) and repo LICENSE
files. This resolves the open license question in `VERIFIER_V0.md` §0.

| Asset | License | Verified via | Use permitted |
|---|---|---|---|
| `lrsbrgrn/HalluGuard-Qwen3-4B` (safetensors) | **Apache 2.0** | HF API 2026-07-29 | Adopt candidate is fully shippable |
| `lrsbrgrn/HalluGuard-Qwen3-4B-GGUF` (2.3GB, Q4-class quant) | **Apache 2.0** | HF API 2026-07-29 | Shippable |
| `lrsbrgrn/HalluGuard-Preferences-76k` | **Apache 2.0** | HF API 2026-07-29 | Trainable (Stream A) |
| `Qwen/Qwen3.5-0.8B` | **Apache 2.0** | HF API 2026-07-29 | Base model OK |
| `lytang/LLM-AggreFact` | **CC-BY-ND-4.0**, gated | HF API 2026-07-29; access granted to `svrnmesh` same day | Eval only. No derivative redistribution; internal benchmarking + reporting numbers on the eval card is fine |
| FaithBench (`vectara/FaithBench` GitHub) | **CC-BY-NC-SA-4.0** | LICENSE file in clone | Eval only, non-commercial terms; internal benchmarking + card numbers with attribution |
| `bespokelabs/Bespoke-MiniCheck-7B` | **CC-BY-NC-4.0** | spec §0 (not re-verified today) | Baseline only, never shippable |

Consequences for the plan:

- **The adopt fallback is real and unencumbered.** HalluGuard-Qwen3-4B being
  Apache 2.0 means the §0 discipline (adopt ships if it wins the card) has no
  legal obstacle. The spec's "license unverified" caveat is closed.
- The GGUF on HF is a ~2.3GB Q4-class quant of a 4B model. Published paper
  numbers (84.0 RAGTruth / 75.7 avg) are presumably bf16 — the baseline run
  should measure both the bf16 safetensors (fidelity) and the shipped GGUF
  (deployment parity), and any gap goes on the card as the quant-delta row.
- Benchmark sets carry ND/NC terms: they gate *training-data* use and
  redistribution, not internal evaluation. Neither may ever enter a training
  stream — which the contamination pass enforces mechanically anyway.

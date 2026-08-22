# drb2 — the DRB-II scorer and flight driver (order `deep-research-t7a`)

The first DRB-II measurement for the deep-research loop. Order,
pre-registration, and verdict rules live in
`research/deep-research/adversarial/pre-registration.md` (section
"T7a ... DECLARATION" + named amendments N1, N2, + selection execution
record). This directory is the executable instrument; the pre-registration
is the contract.

## What this is

- `vendor/` — the OFFICIAL DRB-II evaluation protocol, vendored BYTE-EXACT
  from the pinned clone (`imlrz/DeepResearch-Bench-II` @
  `087c1b8d4a0ed46fd3dd8615a0b5e93ce3acf6f8`, cloned 2026-08-19):
  the judge prompt template, the parse/validate functions, the score
  aggregation, and the chat-completions client. `SHA256SUMS` hashes the
  four files; `PROVENANCE.md` names the source lines and the verification
  commands. Never edit these files in place.
- `drb2-score.py` — the scorer. Imports the vendored protocol directly
  (one implementation, ARCH §10.6). Judge: the local daemon at
  127.0.0.1:9741, model `Qwen3.8-27B-UD-Q6_K_XL` (27B pin; the 122B window
  is rung 2, seat-routed). Per-rubric rows persist as the official
  `{"model", "idx", "result"}` JSONL lines. Adds: seeded cluster bootstrap
  (10k), the four-verdict Leg A on the paired per-task delta, the
  pre-registered calibration channels M1/M2/M3, and a mock-judge selftest
  (`--selftest`, no daemon). Since 2026-08-20 the instrument carries
  pre-registered amendment N5 (pre-registration.md T7a section): ONE
  typography-normalization function (casefold, whitespace removal, quote
  unification, markdown-decoration strip) applied to both sides before the
  VENDORED validator (which remains the sole decider — letter-level
  substitution still fails), plus parse robustness (think-block strip,
  last-fence attempt, then the N1 fallback). "Vendored-byte-exact" now
  reads "vendored + pre-registered amendments (2026-08-20)".
- `select-drb2-sample.py` — content-blind seeded sample (8 of 64 en
  non-NC tasks, 22 themes weighted inverse to Perplexity's Table 8
  theme totals). Reads only idx/language/theme/license.
- `run-drb2-arm.py` — the flight driver. Flies the loop AS-IS (web,
  12 search / 12 fetch / max-3 rounds) on the 8 drawn tasks, enforces
  the pinned-binary sha256, propagates every flight's exit code, and
  copies `dr-*/report.md` to `reports/ours/idx-N.md`.
- `selection.json` — the pinned draw (seed string is the audit key).
- `fixtures/` + `reports/{Perplexity-Research,Qwen-3-Max-DeepResearch}/`
  — the official shipped reports for the 8 sampled tasks, downloaded
  2026-08-19 from the HF dataset (no scores in the dataset; verified).
  PDFs (Perplexity idx-126; all Qwen) are extracted with
  `pdftotext -layout` — amendment N2, recorded in `fixtures/MANIFEST.json`.
- `results/` — per-report score JSONL + `drb2-report.json` (the
  instrument report: Leg A/B/C, calibration, instrument block).

## Pre-registered named deviations (nothing silent)

1. Paper truncation 45,000 chars (official 150,000) — the 27B judge's
   measured context; same budget for all report sets.
2. Chunk size 4 — amendment N4 (2026-08-19): the shared daemon's MTP
   inference deadline is 300s (SOVEREIGN_INFERENCE_TIMEOUT_SECS default;
   a 10-item batch measured at 252.6s passed, the same batch was killed
   at 300s on the next run), so 4-item batches (~90-150s measured) are
   the operating size. Transport only; per-item scoring unchanged.
   Env override DRB2_CHUNK_SIZE.
3. Output tokens 16,384 (official 32,768).
4. Retries 5 (official default).
5. `reasoning_effort`: vendored default "medium"; on HTTP 400/422 the
   client strips the field, retries once, and records the event
   (`reasoning_effort_strip_events` in the report).
6. N1 — parse fallback: vendored parser verbatim first; on failure a
   counted `json.loads` fallback (the official `_try_clean_and_load`
   corrupts compact single-line JSON — measured; the official judge
   emits multi-line). Count lands in the report.
7. N2 — fixture PDFs are text-extracted (pdftotext -layout); the
   extracted text is the judged artifact.

## Run order (per the pre-registration)

1. `python3 select-drb2-sample.py` — DONE (record appended).
2. Fixture download + manifest — DONE.
3. `python3 drb2-score.py --selftest` — DONE (mock judge, no daemon).
4. Calibration on the quiet daemon slot (M0 probe + M1/M2/M3) — the
   calibration record lands in the pre-registration BEFORE any scored
   flight.
5. Flight window via the seat (SendMessage to main), then
   `python3 run-drb2-arm.py --via-systemd` (96 searches = 8 x 12).
6. `python3 drb2-score.py --calibrate` — the scored numbers + verdicts.
7. Execution record appended; ONE landing commit (local, no push).

## Verdicts (pre-registered §6)

- Leg A (the primary read): ours vs Perplexity-Research, SAME judge,
  SAME 8 tasks — paired cluster bootstrap CI on the per-task TotalScore
  delta; met if CI_lo > 0, failed if CI_hi <= 0, could-not-judge
  otherwise, never-ran if no scored flight.
- Leg B: ours vs official reference lines (Perplexity 38.58 /
  nvidia-aiq 54.50, GPT-5.5-judged, 132 tasks; en-only 38.03) —
  descriptive with judge-identity + task-set caveats, never a gate.
- Leg C: the -1 channel (blocked_rate) per report set, reported.

## Costs

Web: 96 searches (the operator cap), nothing else paid. Judge calls:
24 reports x <=3 batches on the 27B, sequenced in the quiet slot.

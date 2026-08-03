# Mac migration — outcome, and three corrections to MAC_MIGRATION.md

**Status: the Mac is the training box. All five `MAC_MIGRATION.md §5` checks
pass.** Stream B is local and verified, `orpo-ab` is built, and a 3-step ORPO
run reproduces the M0 throughput family. Nothing further gates M1 or the M2
mix study except a decision about which to run.

Measured 2026-08-02 on the M2 Max (64 GB), mlx-lm-lora / Metal.

---

## 1. The transfer had failed silently — the route was fine, the server root wasn't

`data/stream_b/all/orpo_pairs.jsonl` on the Mac was **460 bytes of HTML**: a
`SimpleHTTP` 404 page, saved under the dataset's filename. `wc -l` reported 18
lines, so every "did the file arrive" check that counts bytes or lines would
have said yes.

Cause: the Halo served `python3 -m http.server 8099` from the **repo root**, but
`MAC_MIGRATION.md §1` route 2 instructs `curl -O http://<halo>:8099/orpo_pairs.jsonl`
— a path that only resolves if the server was started inside
`research/verifier-v0/data/stream_b/all/`. The route itself works; the recipe
omits that the server's CWD defines the URL root.

Corrected pull:

```bash
curl -O http://100.115.12.21:8099/research/verifier-v0/data/stream_b/all/orpo_pairs.jsonl
```

**105,604,439 bytes — ~101 MB, not the ~180 MB §1 estimates.**

### Verified, not assumed

Byte count matched `Content-Length` exactly, and the payload was checked against
`findings/M2_STREAM_B_LABELING.json` — a record committed from the Halo, so this
is an independent cross-check rather than a self-consistent one:

| dimension | committed finding | downloaded file |
|---|---|---|
| rows | 19,019 | 19,019 |
| unparseable / empty-side | 0 / 0 | 0 / 0 |
| by corpus | sep 17,007 · secret-agent 1,830 · saltgrass 182 | identical |
| by kind | 10 kinds, verbatim 3,755 … ocr_garble 145 | identical |
| by label | grounded 10,458 / ungrounded 8,561 | identical |
| grounded share | 0.5499 | 0.5499 |

`sha256(orpo_pairs.jsonl) = dca7221631507806c564e0af4dbea5f8b2c09fda2eea4939051d9adf0f802fca`

**Leave the Halo's `http.server` running only as long as the transfer needs it.**
Rooted at the repo, it serves `.git/`, `.sovereign/`, and `.config/` to the whole
tailnet unauthenticated. That is a much broader surface than the one file, and
it is the reason `beefymac-ops` is an allowlist in the first place.

---

## 2. CORRECTION — the Mac's Stream A was never stale, and never contaminated

`MAC_MIGRATION.md §0` says any `data/orpo-76k` on the Mac is "STALE AND
CONTAMINATED … Overwrite, do not reuse." **Measured false.**

- `findings/contamination_report.json` (2026-07-29, pre-re-fix) and
  `findings/contamination_report_streamA_refixed.json` (2026-08-01) flag the
  **identical 34 rows** — set difference is empty in both directions.
- Rebuilding with the refixed report produced `orpo-76k` and `orpo-probe`
  **byte-identical** to the Jul 29 build (sha256 on all six split files).

Why §0's own check could not have caught this either way: it asks for
`train == 74674`, `excluded == 34`, `seed == 17` — all three of which the
allegedly-stale build already reported. **The check was not falsifying.** A
report-identity check is: compare the row-id sets of the two contamination
reports, which is what settled it here.

The re-fix was real (note 72b3ab47 — the top line counted evidence collisions
only, missing the claim path); it simply found nothing new on Stream A.

---

## 3. CORRECTION — the `max_prompt_length` truncation risk does not exist on this lane

`MAC_MIGRATION.md §4` carries forward from the Halo: "`max_prompt_length` 2048
truncates 7 of 2000 (0.35%). Re-check on the real sets before M1 — for a
grounding verifier a truncated document is a label the model cannot verify."

The concern is right, and the mechanism is real on the Halo. **It does not
transfer to MLX**, because the two trainers truncate different ends:

- **TRL** truncates the *prompt* at `max_prompt_length`. The prompt is the
  evidence document — cutting it destroys the label. That is the dangerous case.
- **mlx-lm-lora has no `max_prompt_length` at all** (`grep -rn max_prompt` over
  the package: no matches). `iterate_orpo_batches`
  (`trainer/orpo_trainer.py:123`) tail-truncates at `max_seq_length`, which cuts
  the *completion*, never the evidence.

Measured on the real sets (`scripts/measure_truncation.py`, 6,000-row seeded
samples, Qwen3.5 tokenizer):

| | prompt p50 / p99 / max | prompt+completion p50 / p99 / max | > 2048 prompt | > 4096 seq |
|---|---|---|---|---|
| **A** (`orpo-76k` train) | 835 / 1,710 / 2,761 | 1,816 / 3,200 / 4,212 | 0.28% | **0.02%** (1 row) |
| **B** (`stream_b` pairs) | 823 / 2,481 / 2,637 | 985 / 2,648 / 3,134 | 1.65% | **0.00%** |

Two things to keep:

1. **On the MLX lane the binding constraint is `max_seq_length` 4096, and
   essentially nothing hits it** — 1 row in 6,000 for A, 0 for B. The §4
   pre-M1 re-check is complete; it is not a gate any more.
2. **B's prompts run 6x hotter against a 2048 prompt cap than A's** (1.65% vs
   0.28%). Harmless here. It becomes live the moment anything runs on TRL —
   the Halo lane, or a rented GPU for M3 — where it would cut evidence out of
   Stream B rows at six times Stream A's rate and quietly bias the mix study
   against B. **If M3 moves off MLX, set `max_prompt_length` ≥ 2688 or drop
   the cap.**

---

## 4. Batch composition — checked, and it is safe for a mix study

`iterate_orpo_batches` sorts the whole dataset by completion length
(`orpo_trainer.py:97`) and cuts batches from contiguous runs, so any single
micro-batch is length-homogeneous. Since B's rows are systematically shorter
(p50 985 vs 1,816), B concentrates into its own micro-batches.

That would be a serious confound — a de-facto A-then-B curriculum — except for
two things that make it a non-issue:

- Batch **order** is re-permuted every epoch when training (`:116`).
- One optimizer step averages 8 micro-batches (`--gradient-accumulation-steps 8`),
  drawn from that permutation, so every gradient step mixes both streams.

This length-sorting is also the likeliest explanation for why the Halo's
"bigger micro-batch was *slower*" finding does not reproduce here: MLX buckets
by length, so padding waste stays low instead of padding every sequence to the
longest in a random batch.

---

## 5. Throughput — reproduced, and the wall-clock table it implies

`§5` check 5 (3-step 0.8B on `orpo-probe`): **40.82 s/it**, exit 0, loss 0.064.

Against the M0 reference of **54.79 s/it** (100 iters / 1:31:19, peak RSS 27 GB
of 64), this is *faster*, not a regression — and 3 iterations is too small a
sample to mean much either way. The M0 log's per-iteration spread was 28.24 –
104.37 s/it; 40.82 falls inside it. Loss 0.064 sits on the M0 plateau
(0.054 – 0.060 from iter 19 onward). **Treat 54.79 s/it as the planning number**
— it is the only one measured over a full run.

At effective batch 32:

| run | train rows | iters/epoch | s/it | wall-clock / epoch |
|---|---|---|---|---|
| **A** (`orpo-76k`) | 74,674 | 2,334 | 54.79 measured | **35.5 h** |
| **A+B** (`orpo-ab`) | 93,693 | 2,928 | ~50 est. (B rows shorter) | **~41 h** |

`data/orpo-ab` is built and verified: train 93,693 / valid 1,000 / test 1,000,
`stream_b_rows` 19,019, `stream_b_share` 0.1988, `stream_a_rows` 76,674.

---

## 6. What this changes about sequencing

`MAC_MIGRATION.md §3` says "M1 is the next milestone, not M3." True, but it
misses that **M1 and the M2 mix study's control arm are the same training run.**

- M1's gate (spec §7) is *pipeline*: train → eval → calibrate → GGUF → rescore
  end-to-end, on Stream A.
- The mix study's A-arm is 0.8B on Stream A, evaluated.

Hold iterations matched between arms and one Stream-A run serves both. The
honest control for a mix study is **equal examples seen**, not equal epochs —
at 2,334 iters that is 1.00 epoch of A and 0.80 epoch of A+B, which is the
comparison you want (only the mixture differs).

A short matched run is defensible on the M0 evidence: ORPO loss fell 0.155 →
0.056 by iter 19 and was flat through iter 100. The signal saturates early, and
the mix study measures a *relative* difference. The Halo's own handoff says the
mix study "wants a short run."

---

## 7. The eval leg was broken, and it would have wasted the whole mix study

**Run before the training: it found two defects that would have made both
mix-study arms unscoreable.** Measured on the M0 probe checkpoint
(`runs/probe-0.8b-orpo/probe-orpo-0.8b-q8.gguf`) served via llama-server on
:8089, LLM-AggreFact slices, `runs/m1-evalleg-probe/`.

| protocol | macro BAcc strict | macro BAcc tolerant | hit token cap | unparseable | throughput |
|---|---|---|---|---|---|
| thinking ON (committed baseline protocol) | **0.00** | **0.00** | **55 / 55** | 55 | 7.3 items/min |
| thinking OFF (`--no-think`) | 10.88 | **55.03** | 3 / 163 | 2 | 83 items/min |

### Defect 1 — the 0.8B degenerates into a repetition loop, 100% of the time

Under the harness's protocol (temperature 0, no repetition penalty, thinking
on) the 0.8B reaches a correct answer in ~200 tokens and then repeats itself
until the cap. Verbatim tail of a real response:

```
Is "in County Clare" supported? Yes.
Is "in northwest County Clare" supported? Yes.
Is "in County Clare" supported? Yes.        ... x N until max_tokens
```

**55 of 55 items hit the cap. Zero verdicts. BAcc 0.00.** No parser can rescue
a model that never leaves `<think>`.

This is the same failure that gave the 4B baseline its 7.6% parse-fail rate —
the 4B is simply far less prone to it. It scales inversely with model size,
which is exactly backwards from where the 0.8B lane needs it.

What was tried, on 4 items each: `repeat_penalty 1.1` fixed 1 of 4;
`temperature 0.6 / top_p 0.95` (Qwen's recommended thinking sampling) fixed 0
of 4; **disabling the thinking block fixed 4 of 4**, at 78–101 completion
tokens instead of 2,560.

### Defect 2 — the parser threw away correct verdicts over closing-tag typos

`ANSWER_RE` required a fully well-formed
`<answer><classification>…</classification><justification>…</justification></answer>`
block. The 0.8B emits the right classification and then malforms the wrapper:

```
<classification>GROUNDED</classification>
<justification>...matches the document...</justivation>     <-- typo, and no </answer>
```

and with thinking off, a JSON body instead of tags:

```
<answer>
{"classification":"GROUNDED","justification":"...supported..."}
```

Both are correct verdicts. Both scored as parse failures. **Only 31 of 163
rows (19%) were strictly well-formed** — the rest split `tag` 75, `json` 55.

Strict BAcc of **10.88 is below chance on purpose**: a parse failure is scored
as the *wrong* label, so a high failure rate drives BAcc toward 0, not toward
50. Two subsets scored exactly 0.00. Read that as "no measurement," never as
"the model is worse than a coin."

### What changed in `scripts/eval_grounding.py`

Additive, so every committed baseline stays reproducible from the same code:

- **`parse_verdict` is untouched and still strict.** It remains the column
  BASELINES.md's headline was measured with.
- **`parse_verdict_tolerant`** falls back to a `<classification>` tag, then a
  `"classification":` JSON key, and reports which path hit (`strict|tag|json`).
  It reads the same window — only after the last `</think>` — so a model
  reasoning about the categories still cannot leak a verdict. Both negative
  cases are pinned in `scripts/test_eval_grounding.py` (9/9 pass).
- **`results.jsonl` gains `pred_tolerant`, `cls_tolerant`, `parse_mode`**;
  `summary.json` gains `subsets_tolerant`, `macro_avg_bacc_tolerant`, and a
  `parse` block splitting strict failures / tolerant failures / rescued /
  cap-hits. Cap-hits are counted separately from parse failures so the two
  causes above never get conflated again.
- **`summary.json` records the protocol** (`max_tokens`, sampling overrides).
  A `--no-think` run is not comparable to a thinking run, and that fact now
  travels with the result instead of living in a shell history.
- **Raw responses persist to `responses.jsonl`** (`--no-save-responses` opts
  out). The 4B baseline stored only parsed verdicts, so quantifying defect 2
  against it required re-running the model — 6 hours to answer a question that
  should have been a 2-second offline re-parse. That is the glassbox failure
  underneath both defects.
- New flags: `--repeat-penalty`, `--no-think`.

### What this does NOT establish

**55.03 is not a quality number.** It is a 100-step probe on 2,000 rows, over a
165-item slice, under a protocol (`--no-think`) that differs from the committed
4B baseline. It must not be compared to the 70.77 / 76.76 baseline. Its only
job was to prove the leg runs end to end, and it does.

### Consequence for the mix study

Run both arms with `--no-think`, identical `--per-subset` and `--seed`, and
read `macro_avg_bacc_tolerant`. Report strict alongside it — an unparseable
verdict really is a failed verification in production, so strict stays the
floor the fleet experiences; it just cannot be the *headline* for a model whose
formatting is this shaky.

Cost side-effect worth banking: `--no-think` runs at **83 items/min vs 7.3**
(11x). A full 2,200-item LLM-AggreFact card drops from ~5 hours to ~27 minutes,
so both arms can be scored on the full benchmark instead of a slice.

---

## 8. Still true, still open

- **The 4B memory probe (§4) has not been run.** It remains the one unmeasured
  thing gating M3, and §4's reasoning for why it should fit is unchanged.
  `Qwen/Qwen3.5-4B` is **not** in the Mac's HF cache — ~8 GB to fetch.
- Stream B is **19,019 pairs against a spec floor of 20,000** (short by 981).
  `M2_STREAM_B_VOLUME.md §8` recommends accepting; closing it needs ~250 more
  SEP windows (900 of 93,984 used).
- `ocr_garble` is starved at a 6.6% keep rate, by design — none of these corpora
  were OCR-ingested, so the corruption is synthetic noise over clean text and
  training on it would teach typo == hallucination.

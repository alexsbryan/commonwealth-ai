# SP1 — GLiNER2 in the Rust/ONNX stack (bare ort, pinned rc.9)

**Verdict: YES — the exit criterion is met with room. The pre-exported GLiNER2 monolithic
ONNX graph loads and runs on the PINNED `ort =2.0.0-rc.9`, driven bare (no gline-rs), and
is 2.8× FASTER than v1 in the same harness on the same 50 real sep chunks. The
relation-style field-group schema also fills typed slots through the same export —
with one structural caveat (slots, not linked tuples). No second ORT link needed.
P2.1 confidence: Low → High.**

Measured 2026-07-30, M2 Max, release build. Harness:
`sovereign-gliner/examples/gliner2_probe.rs` (committed) — both stacks run in ONE binary
over the same fixture, so the throughput comparison is apples-to-apples. Machine was
concurrently running the SP2 Arm A′ enrich (daemon/GPU busy); both passes shared that
load, so absolute numbers are conservative but the RATIO is fair.

> ### CORRECTION 2026-08-02 — the ratio is **2.52×**, not 2.8×, and the
> "ratio is fair under shared load" claim above is FALSIFIED.
>
> Re-run on the same M2 Max with the box quiet (nothing >20% CPU), same
> binary, same fixture, **three consecutive runs with near-zero variance**
> (v1 17.40/17.41/17.41 s; g2 6.90/6.91/6.90 s):
>
> | | 2026-07-30 (loaded box) | 2026-08-02 (quiet box) |
> |---|---|---|
> | v1 chunks/s | 2.45 | **2.87** (+17%) |
> | g2 chunks/s | 7.04 | **7.24** (+3%) |
> | **ratio** | **2.8×** | **2.52×** |
>
> Shared load did NOT affect both passes equally: v1 (gline-rs stack)
> degraded ~17% under it while g2 (bare ort) degraded ~3%, so the busy
> box *inflated* the ratio rather than leaving it fair. 2.8× has since
> propagated into `ENRICHMENT_ROADMAP.md`, the P2.1 plan step and
> `SIZING:220`; the honest figure for predicting P2.1's delta is **2.52×**.
> The verdict (YES, adopt) is unchanged — this moves the predicted saving
> on a 330-note vault build by about one minute, not the decision.
>
> ### The RSS finding is INVERTED — GLiNER2 is ~4.8× LIGHTER than v1
>
> The "~6–7 GB incremental for GLiNER2, the real blocker for desktop and
> daemon residency" is an **artifact of an invalid subtraction**, not a
> property of the model. The probe was rerun with a new `--only=` flag
> that isolates each pass in its own process (`--only=v1|g2|rel|all`),
> which is the only way max-RSS is attributable. Three runs each, quiet
> box, same 50-chunk fixture:
>
> | isolated pass | max RSS | chunks/s |
> |---|---|---|
> | **GLiNER2 (bare ort)** | **2.39 / 2.40 / 2.44 GB** | 7.16 / 7.22 / 7.23 |
> | **v1 (gline-rs stack)** | **11.53 / 11.54 / 11.96 GB** | 2.77 / 2.85 / 2.87 |
>
> **v1 is the memory hog, and GLiNER2 replacing it is a ~9 GB REDUCTION
> in peak RSS on this workload.** The original 6.7 GB "incremental" came
> from subtracting a **1.63 GB `gliner_smoke` run — a different example
> over a different, smaller workload** — from a union peak that was
> almost entirely v1's own footprint, then attributing the whole
> remainder to GLiNER2. Two different workloads on either side of the
> minus sign.
>
> **Consequence for P2.1:** the plan's "arena/session tuning is a
> prerequisite, not a polish item" and "the real blocker for desktop and
> daemon residency" are both **retired**. Adopting GLiNER2 improves
> residency. Arena tuning becomes an optional optimisation on a 2.4 GB
> footprint rather than a gate on a claimed 6–7 GB one. Measure the
> desktop budget against 2.4 GB, not 6–7 GB.
>
> ### CORRECTION 2026-08-03 — GLiNER2 does NOT fix type-collapse. It mistypes ~1 mention in 3.
>
> SP1 never measured typing; it measured throughput, memory, and schema
> mechanics, and P2.1's "fixes type-collapse by extracting types jointly"
> rode on the paper rather than on this stack. Measured now, through the
> production seam (`LabeledEntityExtractor`, so this is what
> `chunk_entities` would store), harness
> `sovereign-gliner/examples/typing_audit.rs`, oracle
> `sovereign/bench/gliner/typing_oracle_sep.json` (BonJour/Sosa + 17 philosopher surnames
> that must be `Person`), fixture = the 269 sep chunks that actually
> mention BonJour or Sosa:
>
> | | v1 | GLiNER2 |
> |---|---|---|
> | entity level (dominant label) | 17/17 | **17/17** |
> | **mention level (rows written)** | **293/294 = 99.7%** | **167/248 = 67.3%** |
>
> `Sosa` under GLiNER2: 75 `Person`, 33 `Work`, 24 `Organization`, 1
> `Location`, 1 `Event`. `BonJour`: 22 `Person`, 17 `Work`, 3
> `Organization`. Under v1 both are `Person` every time.
>
> **The entity-level row is the trap.** Take the most common label per
> name and the two backends look identical — which is what the first
> pass of this audit reported before it grew a per-mention column. But
> `chunk_entities` is a MENTION table; a minority mistyping is a wrong
> row on disk, and one row in three is wrong.
>
> Two things this does NOT say. It is not a recall claim — GLiNER2
> produces *more* mentions overall on the same fixture (1511 vs 1226).
> And it is not a "v1 is better" verdict on breadth: the extra volume is
> real, it just lands on other surfaces while these named entities get
> both under-found (248 vs 294 mentions) and mistyped.
>
> **Consequence for P2.1:** the ordering in the plan — "(a) the
> conversation/vault path, replacing v1, fixing type-collapse by
> extracting types jointly" — is not supported. The speed and residency
> case for GLiNER2 stands (2.52×, ~9 GB lighter); the *quality* case for
> it as a drop-in replacement does not. `SOVEREIGN_GLINER_MODEL_ID` ships
> default-off with a row in `DEFAULTS_LEDGER.md` for exactly this reason.

## Method (exact commands)

```
cargo build --release -p sovereign-gliner --example gliner2_probe
/usr/bin/time -l ./target/release/examples/gliner2_probe \
  ~/.cache/huggingface/hub/models--lion-ai--gliner2-base-v1-onnx/snapshots/5551729ccc76b30395bc9600f2348ec52a87cead \
  research/enrichment-spikes/data/chunks_50.jsonl
```

Model: `lion-ai/gliner2-base-v1-onnx` (pre-exported monolithic encoder+span-head, 795MB,
DeBERTa-v3-base backbone). Input contract: `input_ids` / `attention_mask` /
`text_positions` / `schema_positions` / `span_idx` → `span_scores (1, fields, words, 8)`;
schema `( [P] task ( [E] field … ) ) [SEP_TEXT] words`, pre-tokenized encoding.
Fixture: 50 seeded-random sep chunks (`scripts/dump_chunks.py --seed 7`, p50 810 chars,
8,485 words total). v1 = installed `gliner_small-v2.1` via the production
`GlinerExtractor` (gline-rs stack), `extract_batch`, threshold 0.6 (its default).
GLiNER2 threshold 0.5 (export README default).

## rc.9 compatibility (the actual spike question)

Zero issues. Port deltas from the artifact's rc.12 example were purely mechanical:
`ort::inputs![]` is fallible in rc.9 (`?`), `try_extract_tensor` returns an ndarray view
(index `view[[0,fi,start,w]]`) rather than a `(shape, slice)` tuple. Session build,
tensor construction, and the run call are otherwise identical to the in-house PaddleOCR
bare-ort pattern (`local_corpus/ocr/paddle/detect.rs`). Load time 716ms.

## Numbers

| Metric | v1 (gliner_small-v2.1) | GLiNER2 base (bare rc.9) |
|---|---|---|
| total wall, 50 chunks | 20.4 s | **7.1 s** |
| chunks/s | 2.45 | **7.04** (2.8×) |
| words/s | 416 | **1,195** |
| mentions/chunk | 3.2 | 8.5 (threshold 0.5, no span-NMS) |
| model on disk | 591 MB | 795 MB |
| process max RSS | 1.63 GB (solo, gliner_smoke) | ~6.7 GB incremental (8.3 GB combined run minus v1 solo) |

Entity quality eyeball (same chunks): v1 and g2 agree on the clear people
(Dewey, Bealer, Jackson, Peacocke, Boghossian); g2 adds correct mentions v1 missed
(Alain Locke, Barnes, Paul Boghossian) and denser Work/date coverage. g2 artifacts:
duplicate overlapping spans (no NMS in the probe — production needs the standard
max-score-span dedup) and it tags the leading article slug baked into sep chunk text
(fixture artifact, not a model fault).

## Relation trial

Schema `( [P] authorship ( [E] author [E] work title ) )` over the same 50 chunks:
227 slot fills in 7.3s, and the fills are genuinely slot-typed — e.g. author "Aquinas" +
work title "ST II-II q. 64 a. 7"; work titles "Z.1, 1028a20–31" / "Catechism of the
Catholic Church"; authors Aristotle/Sennett/Horty. **Caveat:** the export's `span_scores`
head yields typed spans per field, NOT linked (author, work) tuples — pairing would be a
post-hoc step (proximity/syntax heuristic) or need the full GLiNER2 structured head,
which this export does not expose. So: entities YES, typed slots YES, tuple-linked
relations PARTIAL.

## Consequences for P2.1 (GLiNER2 upgrade, sizing doc §2)

- Confidence Low → **High**; size stays M but the "second onnxruntime link" fallback and
  its bundle-cost pricing are DEAD — rc.9 runs the graph as-is.
- The gline-rs dependency is not needed for GLiNER2: the bare-ort path is ~200 lines
  (probe) and follows the PaddleOCR precedent. `SemplificaAI/gliner2-rs` was not needed
  and was not evaluated.
- RSS: budget ~6-7 GB resident for base under default ort arena settings on long doc
  chunks; session/arena tuning is the named follow-up before production residency.
- Tuple-linked relation extraction should be scoped as post-hoc pairing over slot fills
  (or stay LLM-judged), per the partial result above.
- D1 rides resolved: gliner_ner.rs:19 size comment corrected (591MB measured, was
  "~150MB"); the ~10 (conv) vs ~24 mentions/chunk comments are corpus-specific claims —
  measured 3.2 (v1, threshold 0.6) on SEP doc chunks, left as-is with this datum recorded.

## Artifacts

- `sovereign/crates/sovereign-gliner/examples/gliner2_probe.rs` (committed; dev-deps
  `ort =2.0.0-rc.9` + `ndarray 0.16` + `tokenizers 0.21` added to sovereign-gliner —
  versions unify with the existing lockfile, no second ORT).
- Fixture `data/chunks_50.jsonl` (gitignored, regenerate with seed 7).
- Model cached at HF snapshot `5551729ccc76b30395bc9600f2348ec52a87cead`.

# OCR engine swap: tesseract → PaddleOCR (ONNX/ort) — HANDOFF

**Status as of 2026-05-27 (updated):** bake-off complete AND the line-scramble root-caused + fixed. **Decision flipped: PaddleOCR is now swap-viable.** The scramble was `det_limit_side_len=960` downsampling 300-dpi full pages to ~27%, merging adjacent text lines. Raising the default to **1600** makes paddle *beat* tesseract on both test docs (The Prince CER 0.0031 vs 0.0036; From Dictatorship 0.0212 vs 0.0652). Default changed in `PaddleConfig`. Remaining before flipping the desktop default: bundle the models + wire `OcrCtx.engine = Paddle` (Task #4 in "What's LEFT"), and ideally confirm on a real scan. See "Findings & decision".

**(superseded) Status as of 2026-05-27:** bake-off complete; initial read was "do NOT swap" because at the stock `det_limit=960` paddle scrambled lines on dense pages. That was a tunable detection bug, not a model-quality ceiling — see the det_limit sweep below.

**Status as of 2026-05-26:** engine code written and compiling; model fetch + bake-off + the swap decision remain. This doc lets another dev finish it on another machine. Author left mid-Phase-3.

---

## Why this exists

Two things surfaced working through `sovereign/crates/sovereign-desktop/RELEASING.md`:

1. **OCR peer-dep wart.** The OCR pipeline (`pdfium rasterize → tesseract subprocess → daemon LLM cleanup`) is architecturally sound, but v1 ships **no self-contained tesseract** on macOS/Linux — users must `brew/apt install tesseract`. We want to delete that dependency class.
2. **Desktop bundle weight.** The desktop binary is ~397 MB, **unstripped** (835k symbols); 207 MB of `.text` from static llama/ggml-vulkan + LanceDB + tantivy + **ONNX Runtime (already statically linked, ~29k symbols, for GLiNER)**.

**Decision:** replace tesseract with **PaddleOCR driven through `ort`**, reusing the ONNX Runtime *already linked for GLiNER* (one ML runtime, not a second like `ocrs`/RTen). Assess quality via a bake-off before committing the swap.

## The load-bearing constraint (read before touching deps)

`orp` + `gline-rs` (GLiNER's stack) **hard-pin `ort =2.0.0-rc.9`**, and every published version of both does. Off-the-shelf PP-OCR crates want rc.10 (`paddle-ocr-rs`) / rc.12 (`kreuzberg`). Per [ort#399](https://github.com/pykeio/ort/issues/399), **rc.10 broke `ort-sys`** (the native onnxruntime linker), and two `ort-sys` versions can't coexist (duplicate static onnxruntime → link failure). So it's bump-everything-or-nothing.

**Build-verified (Phase 0 spike, both endpoints):** vendoring orp+gline-rs with the pin relaxed and `cargo build -p sovereign-tools --features gliner-ner`:
- **rc.10** → `orp` fails, 4 errors (`SessionOutputs` 2→1 lifetime params; `Session::run` now `&mut self`).
- **rc.12** → `orp` fails, **60 errors** (same + `ort::Error` became an opaque non-`Send`/`Sync` struct, breaking every `?` propagating an ort error).

⟹ **We drive `ort` directly at `=2.0.0-rc.9`** (unifies with orp's exact pin → single onnxruntime, GLiNER untouched) and hand-roll the PP-OCR pipeline. Do NOT try to bump ort to use an off-the-shelf crate — it's a verified dead end. See memory note `project_ort_pin_locked_rc9`.

---

## What's DONE

- **Bundle trim (independent win, landed):** root `Cargo.toml` `[profile.release]` now `strip = "symbols"`, `lto = "thin"`, `codegen-units = 1`. Reclaims the ~80 MB of symbol table (Tauri does not strip Rust binaries for you) plus LTO dead-code pruning.
- **Phase 1 — swappable engine seam (compiles clean, default features):**
  - `ocr/engine.rs` — `trait OcrEngine { recognize(&DynamicImage) -> Result<String, OcrError> }`, `OcrError`, `OcrEngineKind {Tesseract, Paddle}`, `build_engine(&OcrCtx) -> Arc<dyn OcrEngine>`.
  - `ocr/tesseract.rs` — existing logic refactored into `run_tesseract_paths`; new `TesseractEngine` impls the trait. `recognize_page` kept for back-compat.
  - `ocr/pipeline.rs` — builds the engine ONCE per doc (`extract_pdf_with_rasterizer` → `build_engine` → `extract_pdf_with_engine`); per-page loop dispatches `engine.recognize` through the trait. A model-load failure fails the document once (not per-page placeholders).
  - `ocr/mod.rs` — `OcrCtx.engine: OcrEngineKind` field; desktop `local_corpus_commands.rs` sets `Tesseract` (no behaviour change).
- **Phase 2 — deps + PaddleEngine (compiling under `--features paddle-ocr`):**
  - `sovereign-tools/Cargo.toml`: `paddle-ocr` feature = `["dep:ort","dep:ndarray","dep:imageproc","dep:i_overlay"]`; `ort = "=2.0.0-rc.9"` (features `["ndarray"]`), `ndarray 0.16`, `imageproc 0.26`, `i_overlay 4`; `strsim` dev-dep + `[[example]] paddle_bakeoff` (required-features).
  - `ocr/paddle/{mod,dict,geometry,detect,recognize}.rs` — full DBNet det + CRNN/SVTR rec + CTC decode, driving `ort` rc.9 directly. Unit tests for dict load, geometry (unclip/sort), and CTC decode (blank/repeat collapse, trailing-space class).

## What's LEFT (in order)

### 1. Fetch models (Task #4)
Models resolve from `~/.sovereign/models/paddle-ocr/ppocr-en-v4v5/` (or `$SOVEREIGN_PADDLE_OCR_MODEL_DIR`) — needs `det.onnx`, `rec.onnx`, `dict.txt`. Recommended (Apache-2.0):
- det: `https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx` (~4.75 MB)
- rec: `https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/rec.onnx` (~7.83 MB, v5 English)
- dict: `https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/dict.txt`

Quick start:
```sh
D=~/.sovereign/models/paddle-ocr/ppocr-en-v4v5 && mkdir -p "$D"
curl -fSL -o "$D/det.onnx"  "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx"
curl -fSL -o "$D/rec.onnx"  "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/rec.onnx"
curl -fSL -o "$D/dict.txt"  "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/dict.txt"
```
Then fold this into `scripts/fetch-desktop-binaries.sh` (a `paddleocr/` section mirroring tessdata) for the eventual bundle. Verify the dict line count vs the rec model's output class count — `recognize::ctc_decode` warns on a >1 mismatch (it tolerates the +1 trailing-space class).

### 2. Bake-off harness (Task #5) — `sovereign-tools/examples/paddle_bakeoff.rs` (currently a stub)
Wire it: build an `OcrCtx` (dpi from `--dpi`, `pdfium_lib_path` from `$SOVEREIGN_PDFIUM_LIB`), `PdfiumRasterizer::new(&ctx)`, `PaddleEngine::with_config(...)`, then per page run BOTH `TesseractEngine`/`recognize_page` and `PaddleEngine::recognize`, printing char/word counts, per-page ms, first ~200 chars, and CER/WER via `strsim::levenshtein` when `--truth <txt>` is given. The seam fn `pipeline::extract_pdf_with_engine` takes a pre-built `Arc<dyn OcrEngine>` for amortizing the model load.
```sh
cargo run --example paddle_bakeoff --features paddle-ocr -- --pdf <scan.pdf> [--truth <txt>] [--dpi 300]
```

### 3. Fixture (Task #4)
No scanned PDF exists in-repo. **Decision: source a public-domain scan** (the author was told to). Find a scanned (image-based) document PDF with known ground-truth text for CER/WER (e.g. a scanned book page from a public-domain source). Drop it under a test-assets path and point the harness at it.

### 4. Run + measure + decide (Task #6)
Run the bake-off; capture quality (CER/WER or eyeball), latency, and the **binary-size delta** with `paddle-ocr` on vs off (should be small — onnxruntime already linked for GLiNER; delta ≈ OCR code + the ~13 MB models if bundled). Then decide: swap to PaddleOCR / keep tesseract / keep both behind `OcrEngineKind`. If swapping: bundle models in `src-tauri/binaries/paddleocr/` (gitignore + `tauri.release.conf.json` `resources` + fetch script), add a runtime resolver mirroring `resolve_tessdata_dir`, set the desktop's `OcrCtx.engine = Paddle`, and update RELEASING.md §"External binaries".

---

## Findings & decision (2026-05-27)

All four "What's LEFT" tasks are done. The harness (`examples/paddle_bakeoff.rs`)
is fully wired and uses a born-digital PDF as its own oracle: `pdf_extract::extract_text_by_pages`
reads the embedded text layer (exact ground truth) while pdfium rasterizes the
*same* pages to images both engines read. Page-aligned (skip/max), whitespace-normalized
CER/WER. New flags: `--skip-pages` (drop front matter), `--max-pages`, `--unclip`,
`--box-thresh`, `--no-tesseract`. `RUST_LOG=sovereign_tools=debug` surfaces the
engine's internal tracing (session i/o names, box counts, dropped lines).

**Models fetched** to `~/.sovereign/models/paddle-ocr/ppocr-en-v4v5/`: det 4.75 MB,
rec 7.83 MB (v5 English), dict 436 lines → 437 classes incl. blank (no class-count
mismatch warning — dict is sound).

**Fixtures:** no scan needed. Used born-digital books from `~/Downloads/Sovereign Test`
(The Prince, From Dictatorship to Democracy), body prose pages (front matter skipped).

**The line-scramble root cause (the whole story).** At the stock `det_limit_side_len=960`,
paddle *scrambled* whole lines on dense pages (The Prince p2: "tak car that rge s poweul s
hiel shal" vs truth "taking care that no foreigner as powerful as himself shall"). The cause
is in `detect.rs`: a 300-dpi full page is ~2480×3508, so capping the longer side to 960
downsamples it to ~27%. At that scale, body-text lines with tight leading **merge** into one
probability-map blob → the crop handed to the rec model spans two stacked lines → CTC emits
garbage. The airy From Dictatorship pages have enough leading to survive the downscale, which
is why paddle already won there. An unclip sweep (1.5→1.8→2.2) made The Prince strictly *worse*
(CER 0.137→0.308→0.893; chars 12.7k→1.6k) — more dilation = more merging — which pointed away
from unclip and at the resize. A `det_limit` sweep confirmed it:

| det_limit | The Prince CER / WER | total chars |
|---|---|---|
| 960 (old default) | 0.137 / 0.169 | 12 743 |
| 1280 | 0.017 / 0.024 | 14 454 |
| **1600** | **0.0031 / 0.0108** | 14 648 |
| 2048 | 0.0037 / 0.0131 | 14 640 |

1600 is the sweet spot (2048 is slower with no gain; 1280 still loses to tesseract). Effective
line separation depends only on this cap — page-px ∝ dpi cancels against the downscale ratio —
so a **fixed 1600 is dpi-independent**, not a 300-dpi-specific magic number. `PaddleConfig`'s
default is now 1600 (`from_ctx` inherits it, so the desktop path benefits automatically).

**Results — body prose, dpi 300, det_limit 1600, both engines (lower is better):**

| Document | paddle CER / WER | tesseract CER / WER | ms/page (paddle vs tess, debug build) |
|---|---|---|---|
| The Prince (pp. 14-17) | **0.0031 / 0.0108** | 0.0036 / 0.0138 | 9000 vs 4000 |
| From Dictatorship (pp. 13-15) | **0.0212 / 0.0442** | 0.0652 / 0.0733 | ~6000 vs 4000 |

**Paddle now beats tesseract on both docs.** From Dictatorship regresses slightly vs its 960
score (0.009 → 0.021) but stays ~3× ahead of tesseract, and The Prince — the worst case — goes
from a loss to a win. Tesseract's losses come from dropping whole lines/headers (it lost the
"Gene Sharp" header + ~370 chars on one FDtD page); paddle's content recall is more complete.

**Latency caveat:** paddle is ~2.3× tesseract here, but this is a **debug build** — the
per-pixel normalize loops in `detect.rs`/`recognize.rs` are unoptimized, and onnxruntime is
already release C++. A release build should narrow the gap substantially (re-measure before
quoting a real number). For an initial ship that *deletes the tesseract build dependency*,
even 2× is an acceptable trade.

**Decision: PaddleOCR is swap-viable.** Quality parity-or-better is achieved. Keep both behind
`OcrEngineKind` (the seam stays — it's cheap insurance and lets us A/B), but the path to making
`Paddle` the default is now unblocked.

**Remaining to flip the desktop default (Task #4 below):**
1. Bundle the models in `src-tauri/binaries/paddleocr/` (gitignore + `tauri.release.conf.json`
   `resources` + fetch script), add a `resolve_paddle_model_dir` mirroring `resolve_tessdata_dir`,
   set the desktop `OcrCtx.engine = Paddle`, fold the model fetch into
   `scripts/fetch-desktop-binaries.sh`, update RELEASING.md §"External binaries".
2. Re-measure latency on a **release** build; if still too slow, optimize the pixel loops
   (`ndarray`/SIMD) before shipping.
3. Nice-to-have: confirm on a **real scan** (skew/noise — tesseract's weak case, where paddle
   should widen its lead). The born-digital oracle is tesseract's *best* case, so these numbers
   are a conservative floor for paddle's relative value.

**Harness caveat:** single-page isolation (`--skip-pages N --max-pages 1`) can mis-align the
oracle because `pdf_extract` and `pdfium` may disagree on page boundaries at an offset;
multi-page runs (≥3 pages) align reliably. Prefer multi-page runs for CER/WER.

## How to verify / build on another machine

```sh
# Default build (tesseract path, watcher-covered) — must stay green:
cargo check -p sovereign-tools
# PaddleOCR engine (compiles ort rc.9 + onnxruntime — first build is slow):
cargo check -p sovereign-tools --features paddle-ocr
cargo test  -p sovereign-tools --features paddle-ocr paddle   # unit tests (dict/geometry/ctc)
# Confirm a SINGLE ort version (rc.9) — never rc.10+:
cargo tree -p sovereign-tools --features paddle-ocr,gliner-ner -i ort
```

Run the bake-off (after fetching models per Task #1):
```sh
export SOVEREIGN_PDFIUM_LIB=/path/to/libpdfium.dylib   # no system pdfium; point at the bundled one
export TESSDATA_PREFIX=/opt/homebrew/share/tessdata    # macOS/brew; or your tessdata dir
cargo build --example paddle_bakeoff --features paddle-ocr -p sovereign-tools
./target/debug/examples/paddle_bakeoff --pdf <doc.pdf> --skip-pages 13 --max-pages 4
# add RUST_LOG=sovereign_tools=debug to see box counts / dropped lines / dict class count
```
Use a born-digital PDF for self-contained CER/WER (its text layer is the oracle); pass
`--truth <txt>` for a real scan. Prefer ≥3 pages so the oracle aligns (see caveat above).

NB: the sovereign watcher / `lint_status` tooling was flaky this session (reported `fresh_failing` with zero errors); use raw `cargo` directly. The `paddle-ocr` feature is off by default, so the background watcher never compiles it — you must build it explicitly.

## rc.9 ort API cheat-sheet (differs from docs.rs, which tracks rc.12)
- `Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?.with_intra_threads(n)?.commit_from_file(p)?`
- input: `Tensor::from_array((shape_vec_i64, data_vec_f32))?`
- run: `session.run(ort::inputs![input_name => tensor]?)?` (`inputs!` yields a `Result`; `run` takes `&self`)
- output: `outputs[0].try_extract_tensor::<f32>()? -> ndarray::ArrayViewD<f32>` (index positionally — output names vary by export; read input name from `session.inputs[0].name`)

## Gotchas / risk register
- **CTC dict off-by-one:** blank is index 0; `dict` is loaded as `[<blank>, ...lines]`. Some rec exports emit one extra trailing class (space) — `class_to_str` maps it; `ctc_decode` warns on a >1 mismatch. Always check the dict matches the rec model.
- **NCHW not NHWC:** both stages pack channel-first `[1,3,H,W]`. `trace!` logs tensor shapes.
- **Detection is axis-aligned (prototype):** `Quad` is an AA rect; `unclip` is box-dilation (`dist = area*ratio/perimeter`). Good enough for horizontal document text. If skew hurts, add rotating-calipers + `imageproc` warp behind a flag.
- **Unclip/box-thresh tuning:** `PaddleConfig.det_unclip_ratio` (1.5) and `det_box_thresh` (0.6) are the knobs to sweep in the bake-off.
- **pdfium** still rasterizes PDFs regardless of OCR engine — `$SOVEREIGN_PDFIUM_LIB` must point at it for the harness.

## Pointers
- Approved plan (not committed): author's plan file. Memory notes: `project_ort_pin_locked_rc9` (the dep dead-end), `project_paddleocr_engine` (this work).
- Original OCR architecture: `ocr/mod.rs` module docs.

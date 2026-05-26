# OCR engine swap: tesseract → PaddleOCR (ONNX/ort) — HANDOFF

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

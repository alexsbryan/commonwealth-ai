# Bundled binaries

This directory holds external binaries the desktop bundles for the OCR
pipeline (folder-drop flow on scanned PDFs):

```
binaries/
├── tesseract-aarch64-apple-darwin              ← macOS arm64 (host or CI-built)
├── tesseract-x86_64-apple-darwin               ← macOS x86_64
├── tesseract-x86_64-unknown-linux-gnu          ← Linux x86_64
├── tesseract-x86_64-pc-windows-msvc.exe        ← Windows x86_64
├── tessdata/
│   └── eng.traineddata                         ← English language pack
└── pdfium/
    ├── libpdfium.dylib                         ← macOS
    ├── pdfium.dll                              ← Windows
    └── libpdfium.so                            ← Linux
```

The `binaries/` directory is `.gitignore`'d (these are large binary
blobs, not source) — every clone needs to populate it before bundling.

## Populating the directory

Run the fetch script. Idempotent — safe to re-run.

```sh
scripts/fetch-desktop-binaries.sh                 # auto-detects host triple
scripts/fetch-desktop-binaries.sh aarch64-apple-darwin   # explicit
```

This fetches PDFium and tessdata automatically; Tesseract is
platform-installed in v1 (the script prints the exact `brew` /
`apt` command for your platform). See
[`../RELEASING.md`](../../RELEASING.md) §"External binaries" for the
full story including the Phase 2 plan to eliminate the platform
dependency.

## Local dev — without bundling

For `cargo tauri dev` on a fresh checkout, the simplest path is env
vars pointing at locally-installed binaries:

```sh
export SOVEREIGN_TESSERACT_BIN=/opt/homebrew/bin/tesseract
export SOVEREIGN_TESSDATA_DIR=/opt/homebrew/share/tessdata
export SOVEREIGN_PDFIUM_LIB=/path/to/libpdfium.dylib
```

`local_corpus_commands::install_ocr_ctx_for_app` honours these env
vars first, before probing the resource directory. With them set,
OCR works in dev without ever populating `src-tauri/binaries/`.

## Release builds

The `tauri build` invocation in `.github/workflows/desktop-release.yml`
passes `--config src-tauri/tauri.release.conf.json`, which adds the
`externalBin` and `resources` entries that pull this directory into
the installer. Plain `cargo check` and `cargo tauri dev` use the base
`tauri.conf.json` (no `externalBin`), so a missing `binaries/` dir is
not an error in dev.

See [`../RELEASING.md`](../../RELEASING.md) for the full release flow.

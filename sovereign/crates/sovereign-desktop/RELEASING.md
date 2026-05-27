# Sovereign Desktop — Releasing

The release process for the Sovereign desktop app: macOS (arm64 +
x86_64), Linux (x86_64), Windows (x86_64). Cuts a draft GitHub Release
with installers for all four targets. v1 is ad-hoc signed on macOS
(runs, but Gatekeeper still warns) and unsigned on Windows; Developer
ID / Authenticode signing + notarization and auto-updates are deferred
(see "Code signing" / "Auto-updates" below). NOTE: GitHub retired the
Intel `macos-13` runner — the `macos-x86_64` matrix leg currently hangs;
see "Build matrix" for the Intel-coverage options.

If something here is wrong or unclear, fix it in the same PR as the
release that revealed the gap. This doc is the authoritative checklist.

---

## At a glance

```
Cut a release:
  1. Bump version           cargo set-version + tauri.conf.json + package.json
  2. Update CHANGELOG.md    user-facing notes
  3. git tag desktop-v0.1.0 && git push origin desktop-v0.1.0
  4. CI runs                .github/workflows/desktop-release.yml
  5. Promote draft release  https://github.com/<owner>/<repo>/releases
```

---

## Versioning

Semantic versioning. The desktop app's version lives in three files
that must move together. Use the bump script — it edits all three and
verifies they agree:

```sh
scripts/bump-desktop-version.sh 0.2.0       # set to an explicit version
scripts/bump-desktop-version.sh patch       # 0.1.0 -> 0.1.1
scripts/bump-desktop-version.sh minor       # 0.1.0 -> 0.2.0
scripts/bump-desktop-version.sh major       # 0.1.0 -> 1.0.0
```

The script writes the new version into:

| File | Field |
|------|-------|
| `Cargo.toml` (workspace root) | `workspace.package.version` |
| `sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json` | `version` |
| `sovereign/crates/sovereign-desktop/package.json` | `version` |

then runs `check-desktop-version.sh` to confirm the trio matches, then
prints the suggested `git add` / `commit` / `tag` / `push` lines.
Nothing is committed automatically — releasing is a deliberate act.

The desktop crate's `src-tauri/Cargo.toml` inherits via
`version.workspace = true`; do NOT add a `version = "…"` line there —
that would diverge from every other crate in the workspace. Bumping
the workspace root version moves all 30+ crates in lockstep, which is
the intended behaviour pre-1.0 (single repo-wide version).

If you only want to verify (no edit), use the check script directly:

```sh
scripts/check-desktop-version.sh            # compare all three
scripts/check-desktop-version.sh 0.2.0      # also require an exact value
```

`check-desktop-version.sh` is also wired into the CI workflow's first
step, so a tag whose three files disagree fails in seconds instead of
after a 30-minute build matrix.

Tag format: `desktop-v<MAJOR>.<MINOR>.<PATCH>`. The CLI and daemon use
their own tag prefixes (`cli-vX.Y.Z`, `daemon-vX.Y.Z`) so all three can
release independently from one repo.

`v0.x.y` while pre-1.0 — minor version bumps may include breaking
changes during this window. After 1.0, follow strict semver.

---

## The release flow

### 1. Pre-release checks

Run before tagging. None are automated — that's the next iteration.

- [ ] `cargo test -p sovereign-tools` — passes locally.
- [ ] `npm run check` (in `sovereign/crates/sovereign-desktop/`) — zero
      Svelte/TS errors.
- [ ] `cargo check -p sovereign-desktop` — clean build, no new warnings.
- [ ] Tauri Rust/JS version alignment. The `@tauri-apps/*` packages in
      `package.json` are pinned with `~X.Y.0` (patch-only) so the
      lockfile stays in lockstep with Cargo's resolved Tauri versions.
      If `cargo update` bumps `tauri` from `2.11.x` → `2.12.x`,
      `tauri-bundler` will fail early with `Found version mismatched
      Tauri packages`. The fix:
      1. Read the Rust-side resolved version from `Cargo.lock`
         (e.g. `tauri = "2.12.3"`)
      2. Edit `sovereign/crates/sovereign-desktop/package.json` to
         match the new minor (e.g. `"@tauri-apps/api": "~2.12.0"`)
      3. Run `npm install` to refresh `package-lock.json`
      4. Commit both files together
      Routine patch bumps (`2.11.1` → `2.11.2`) require no action;
      `npm install` picks them up automatically within the `~` range.
- [ ] Manual smoke test on your dev machine: launch the app, drop a
      folder, confirm ingest completes, ask one question, confirm
      response. Five-minute test.
- [ ] If you touched OCR: run a folder containing a real scanned PDF,
      confirm the offer surfaces and works end-to-end. No env setup —
      the bundled PaddleOCR models + pdfium resolve automatically (boot
      log: `OCR context installed (PaddleOCR)`).
- [ ] Versions bumped via `scripts/bump-desktop-version.sh <new-version>`
      (the script writes the three files and runs the consistency
      check internally — no separate verification needed).
- [ ] `CHANGELOG.md` entry written.

### 2. Cut the release

```sh
scripts/bump-desktop-version.sh 0.2.0       # or: patch | minor | major
git add Cargo.toml \
        sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json \
        sovereign/crates/sovereign-desktop/package.json
git commit -m "chore(desktop): release v0.2.0"
git tag desktop-v0.2.0
git push origin main desktop-v0.2.0
```

The bump script prints these exact lines at the end of its run, so
you can copy from its output instead of typing them.

The push of the tag is what kicks off
`.github/workflows/desktop-release.yml`. If you forget to push the
tag, nothing happens — push it.

### 3. Verify

The workflow takes ~25-40 minutes for the full matrix (PDFium fetch +
4 Tauri builds + matrix recompiles). Watch:
`https://github.com/<owner>/<repo>/actions/workflows/desktop-release.yml`

When green, a **draft** release appears at:
`https://github.com/<owner>/<repo>/releases/tag/desktop-v0.1.0`

Verify each installer exists in the draft:

| Platform | File pattern |
|----------|--------------|
| macOS arm64 | `Sovereign_<ver>_aarch64.dmg` |
| macOS x86_64 | `Sovereign_<ver>_x64.dmg` |
| Linux x86_64 | `sovereign_<ver>_amd64.AppImage` and `.deb` |
| Windows x86_64 | `Sovereign_<ver>_x64-setup.exe` (NSIS) and `.msi` |

Download one matching your machine and install / run it end-to-end
before publishing. **Then** click "Publish release" in the GitHub UI.

If the workflow fails: investigate, fix, retag with the same version
(`-f`) once your fix is on `main`. Don't bump the patch number for a
CI-only failure — the user-facing version didn't change.

---

## External binaries

The OCR pipeline (rasterize → recognize → daemon cleanup) needs two
bundled assets. They're gated by `lc_ocr_available` — if either is
missing the OCR button hides itself and ingest is unaffected.

| Asset | Purpose | Source | Size |
|-------|---------|--------|------|
| `paddle-ocr/ppocr-en-v4v5/{det,rec}.onnx` + `dict.txt` | **OCR engine** (PaddleOCR via the ONNX Runtime already linked for GLiNER) | HF: `SWHL/RapidOCR` (det) + `monkt/paddleocr-onnx` (rec/dict), Apache-2.0 | ~13 MB |
| `pdfium/libpdfium.dylib` / `pdfium.dll` / `libpdfium.so` | PDF page → image rasterization (engine-independent) | `bblanchon/pdfium-binaries` releases | ~7 MB |

PaddleOCR runs **in-process** through `ort` (no second ML runtime —
it reuses GLiNER's onnxruntime) and needs **no platform install**,
which is the whole reason it replaced tesseract (see below). The
desktop selects it automatically: `install_ocr_ctx_for_app` resolves
the bundled models + pdfium and sets `OcrCtx.engine = Paddle`.

### Staging the binaries

`scripts/fetch-desktop-binaries.sh` is referenced by CI but **not yet
written**. Until it exists, stage by hand into
`sovereign/crates/sovereign-desktop/src-tauri/binaries/` (all
gitignored):

```sh
# PaddleOCR models
D=src-tauri/binaries/paddle-ocr/ppocr-en-v4v5 && mkdir -p "$D"
curl -fSL -o "$D/det.onnx"  "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx"
curl -fSL -o "$D/rec.onnx"  "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/rec.onnx"
curl -fSL -o "$D/dict.txt"  "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/dict.txt"

# PDFium (bblanchon/pdfium-binaries — extract lib/<lib_name>)
#   macOS arm64: mac-arm64.tgz → lib/libpdfium.dylib → src-tauri/binaries/pdfium/
```

`tauri.release.conf.json` bundles both as `resources`
(`binaries/pdfium/*`, `binaries/paddle-ocr/ppocr-en-v4v5/*`) into the
`.app`'s `Contents/Resources/`. The runtime resolver probes that path
first, then `~/.sovereign/models/paddle-ocr` for dev machines.

### Why PaddleOCR replaced tesseract

Tesseract was the awkward dependency this swap eliminates. Its brew/apt
builds are dynamically linked to `libleptonica`/`libtiff`/`libjpeg`/
`libpng`, so we couldn't ship a self-contained binary — macOS/Linux
users had to `brew/apt install tesseract` themselves or OCR was
unavailable. PaddleOCR's ONNX models have no such linkage.

The 2026-05-27 bake-off (`sovereign/docs/OCR_PADDLE_ENGINE.md`,
harness `sovereign-tools/examples/paddle_bakeoff.rs`) put PaddleOCR
**at or above** tesseract quality once `det_limit_side_len` was raised
to 1600 (the merge-at-960 bug on dense pages — now the engine default):
The Prince CER 0.0031 vs 0.0036, From Dictatorship 0.0212 vs 0.0652.

Tesseract is **not deleted from the code** — it remains behind
`OcrEngineKind::Tesseract` as a fallback (`install_ocr_ctx_for_app`
uses it when the paddle models can't be resolved, or under
`--no-default-features`). It's simply no longer bundled. If you want a
tesseract-bundling build, restore the `externalBin`/`tessdata` entries
in `tauri.release.conf.json` and stage a (statically linked) binary.

### PDFium

Still required — pdfium rasterizes PDF pages to images regardless of
which engine reads them. `bblanchon/pdfium-binaries` publishes
per-platform tarballs; extract `lib/<lib_name>` into
`src-tauri/binaries/pdfium/`. Stable since 2018, no auth. If a future
ABI change breaks `pdfium-render`, pin the URL to a specific tag.

---

## Build matrix

Four platforms in parallel:

| Platform | Runner | Target triple |
|----------|--------|---------------|
| macOS arm64 | `macos-14` | `aarch64-apple-darwin` |
| macOS x86_64 | `macos-13` | `x86_64-apple-darwin` |
| Linux x86_64 | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` |
| Windows x86_64 | `windows-2022` | `x86_64-pc-windows-msvc` |

Tauri produces these bundle types per platform:

| Platform | Bundle types |
|----------|--------------|
| macOS | `.dmg`, `.app.tar.gz` |
| Linux | `.AppImage`, `.deb` |
| Windows | `.msi`, NSIS `-setup.exe` |

> **⚠️ `macos-13` (Intel) is being retired by GitHub.** Symptom: the
> `macos-x86_64` job sits on *"Waiting for a runner to pick up this job…
> Requested labels: macos-13"* indefinitely — it never gets a runner, so
> the matrix leg hangs (and, because `publish` is `needs: build`, no draft
> release is created even when the other legs are green). This is
> infrastructure, not a build bug. **Intel-coverage options (undecided as
> of 2026-05-25, currently building arm64-only):**
> - **arm64-only** — drop the `macos-13` row; ship Apple Silicon only.
> - **decoupled x86_64** — add a separate `x86_64-apple-darwin` job that
>   *cross-compiles* on a `macos-14` (Apple-Silicon) runner. `fail-fast:
>   false` keeps an x86 failure from sinking arm64. Needs an x86_64
>   `tesseract` sidecar (Rosetta + x86 Homebrew) for the OCR externalBin.
> - **universal2** — one `macos-14` job builds `universal-apple-darwin`
>   (arm64 + x86_64 lipo'd into one `.dmg`). Simplest for users but
>   **couples** arm64's success to the x86 slice: if x86 fails, *no* Mac
>   DMG is produced at all. Also needs the x86 `tesseract` + a fat
>   `pdfium` (`lipo` the two arch dylibs).

Linux runners need: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`,
`librsvg2-dev`, `patchelf`, `libfuse2`, `tesseract-ocr`, `mold`,
`protobuf-compiler`, `libprotobuf-dev`, and the Vulkan-SDK set from
LunarG (`libvulkan-dev vulkan-headers spirv-headers spirv-tools shaderc`).
The workflow installs these. (`protobuf-compiler`/`libprotobuf-dev` are
NOT preinstalled on current runner images — see Troubleshooting.)

`fail-fast: false` is on — one platform failing doesn't kill the
others, so a flaky Windows runner doesn't block the macOS / Linux
artifacts.

---

## Tauri config split

`tauri.conf.json` (committed) has NO `externalBin` / `resources`
entries. Tauri's build script errors when those reference files that
don't exist on disk; in plain dev (`cargo check`, `cargo tauri dev`)
no one has fetched the binaries yet, so they wouldn't exist.

`tauri.release.conf.json` (also committed) is an overlay applied via
`cargo tauri build --config src-tauri/tauri.release.conf.json` that
adds the OCR `resources` (the PaddleOCR models + pdfium). Only release
builds use it; CI invokes Tauri with this flag.

This split is the cleanest available pattern; alternatives (sed-edit
the main config in CI, environment-variable gates, etc.) all add
moving parts.

---

## Code signing

### v1 — ad-hoc (current default)

`tauri.release.conf.json` sets `bundle.macOS.signingIdentity = "-"`, which
makes Tauri **deep ad-hoc sign** the `.app` during bundling (Tauri passes
`-` straight to `codesign`). This is the minimum needed for the app and
its nested binaries to run on Apple Silicon — it is *not* notarization, so
Gatekeeper still warns on first launch (recipients clear quarantine once;
see "Sharing a local build with friends"). No cost, no secrets, no setup.

Windows v1 ships unsigned (SmartScreen warns — "More info" → "Run anyway").

### Phase 2 — Developer ID + notarization (deferred)

When ready to invest (this is what removes the Gatekeeper/SmartScreen
prompts):

### macOS

1. Apple Developer Program membership (~$99/yr).
2. Generate a "Developer ID Application" cert in your developer
   account → `Sovereign-DeveloperID.cer`. Export from Keychain as
   `.p12` with a password.
3. Generate an app-specific password for `notarytool`.
4. CI secrets:
   - `APPLE_CERTIFICATE` — base64 of the `.p12`
   - `APPLE_CERTIFICATE_PASSWORD`
   - `APPLE_SIGNING_IDENTITY` — e.g. `"Developer ID Application: Your Name (TEAMID)"`
   - `APPLE_ID` — your Apple ID
   - `APPLE_PASSWORD` — the app-specific password
   - `APPLE_TEAM_ID`
5. Update `tauri.release.conf.json` `bundle.macOS.signingIdentity` and
   add notarization config.
6. Workflow: import the cert into the runner's keychain before
   `tauri build`, then run `xcrun notarytool submit` on the resulting
   `.dmg` post-build.

The official Tauri action <https://github.com/tauri-apps/tauri-action>
handles most of this.

### Windows

1. Acquire an EV or OV Authenticode cert ($200-700/yr depending on
   issuer). EV gets you instant SmartScreen reputation; OV requires
   ~6 months of downloads to build reputation.
2. CI secrets:
   - `WINDOWS_CERTIFICATE` — base64 of the `.pfx`
   - `WINDOWS_CERTIFICATE_PASSWORD`
3. Update `tauri.release.conf.json` `bundle.windows.certificateThumbprint`
   and `digestAlgorithm`.
4. Workflow: run `signtool sign` on the resulting `.msi` and `-setup.exe`
   post-build.

### Linux

No system-level signing requirement. Optional: GPG-sign the `.deb` via
`debsigs` or publish a signed `.AppImage.zsync` for AppImage updaters.
Not worth doing for v1.

---

## Auto-updates

Wired via `tauri-plugin-updater`. Architecture:

```
   desktop app                svrnme.sh                    GitHub Releases
  +------------+          +----------------+              +-------------+
  | updater    |  -- 1 -> | api/desktop/   |  --- 2 --->  | desktop-v*  |
  | plugin     |          | updater (Edge) |              | latest tag  |
  | (embedded  |          |                |              | + assets    |
  |  pubkey)   |  <- 4 -- | manifest JSON  |  <-- 3 ---   | + .sig files|
  +------------+          +----------------+              +-------------+
       |                                                         ^
       | -------------- 5: download artifact + .sig --------------|
       |                                                         |
       | 6: verify signature against embedded pubkey, install, restart
```

1. Plugin polls `https://svrnme.sh/api/desktop/updater/{target}/{version}`
2. Edge fn queries GitHub Releases for the latest `desktop-v*` tag
3. Reads the per-platform artifact + its `.sig` sidecar from the release
4. Returns 204 (up-to-date) or 200 + manifest JSON
5. On `Some(update)`, plugin downloads the artifact + signature
6. Plugin verifies the sig against the pubkey baked into the app at
   build time. Verified → install + restart. Unverified → reject.

### One-time setup (do this BEFORE cutting any updater-capable release)

The pubkey is embedded in the app at build time, so v0.1.0 must ship
with the real pubkey, OR it will be unable to consume any future
signed update. If you cut v0.1.0 with the empty placeholder
(`plugins.updater.pubkey: ""` in tauri.conf.json), every user who
installed v0.1.0 will need to manually download v0.2.0 once — the
upgrade path only auto-engages from v0.2.0 onwards.

#### Generate the keypair

On your local machine, never in CI:

```sh
cargo tauri signer generate -w ~/.tauri/sovereign-updater.key
```

This prints two things:
- A **password** prompt — pick a strong one, save it in a password
  manager. You'll paste it into GitHub secrets.
- A **base64-encoded public key** — copy it.

#### Wire the pubkey into the app

Edit `sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json`,
replace `plugins.updater.pubkey: ""` with the base64 public key from
the previous step.

#### Wire the private key + password into CI

Repo Settings → Secrets and variables → Actions → New repository
secret. Add two:

| Secret name | Value |
|---|---|
| `TAURI_UPDATER_PRIVATE_KEY` | Contents of `~/.tauri/sovereign-updater.key` (the entire base64 blob) |
| `TAURI_UPDATER_PRIVATE_KEY_PASSWORD` | The password you set during generate |

`.github/workflows/desktop-release.yml` reads these as
`TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
env vars during `cargo tauri build`, which writes `.sig` files next
to each updater artifact.

#### Wire the GitHub repo into the manifest endpoint

The Vercel project for svrnme.sh needs two env vars (Project →
Settings → Environment Variables):

| Env var | Value | Required |
|---|---|---|
| `GITHUB_OWNER` | repo owner (e.g. `alexbryan01`) | yes |
| `GITHUB_REPO` | repo name (e.g. `commonwealth-ai`) | yes |
| `GITHUB_TOKEN` | a `repo`-scoped PAT for higher rate limit | optional but recommended once install base grows past ~60 daily-active updaters |

Redeploy svrnme.sh after setting them.

### Per-release flow (after one-time setup is done)

Identical to the regular release flow. The signing keys are pulled
from secrets automatically; no manual signer steps. After publishing
the GitHub Release:

1. Latest tag is now visible to the manifest endpoint within ~60s
   (Edge cache TTL).
2. Existing installs hit "Check for updates" → see the new version
   → confirm → download + verify + restart.

### The private key is load-bearing

If you lose it:
- You can generate a new keypair, but every existing install will
  refuse the new signature.
- The only recovery is shipping a manually-distributed installer
  with the new pubkey baked in, which users have to find on their
  own (the in-app updater won't help — it's signing-locked to the
  old key).

Back it up. The password manager entry holding it is the most
load-bearing artifact of the release process.

### Things this doesn't do (intentionally)

- **No background install.** Updates apply only when the user clicks
  through the in-app prompt. Quieter is better for "tool you own."
- **No staged rollout.** Every user who polls sees the manifest as
  soon as the GitHub Release leaves draft state. If you want
  staged ramp, gate the `releases.find()` call in `api/desktop/updater.js`
  on a percentage by hashing the target+IP+version.
- **No rollback channel.** If a release is bad, mark it as draft on
  GitHub → manifest endpoint stops serving it → no new installs
  upgrade. Users who already upgraded need a fresh download to roll
  back; there's no downgrade path in the updater plugin itself.

---

## Local rehearsal

Two driver scripts run the same build the GitHub Actions workflow runs,
locally — so you can iterate on Tauri config / `.cargo/config.toml` /
llama-cpp-sys-4 linkage / Vulkan bundling without 30-minute round-trips
through CI.

### Linux

Containerized (mirrors the `ubuntu-22.04` runner with Vulkan SDK +
mold + Tauri + Node 20 + tesseract pre-installed):

```sh
scripts/build-desktop-linux.sh                  # unsigned, ~10min first run
scripts/build-desktop-linux.sh --rebuild        # force container image rebuild
scripts/build-desktop-linux.sh --shell          # drop into the container at /work

# With updater signing:
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/sovereign-updater.key)" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... \
    scripts/build-desktop-linux.sh
```

Outputs land at `target-container-linux/x86_64-unknown-linux-gnu/release/bundle/`
(a separate target dir from your host's `target/` so container compiles
don't stomp on host compiles). Container image: `sovereign-desktop-linux-build:latest`.
Containerfile lives in `sovereign/crates/sovereign-desktop/containerfiles/`.

Runtime: `podman` preferred (Fedora native, rootless), `docker` fallback.

### macOS

Runs natively on the Mac — GitHub's `macos-14` / `macos-13` runners use
real Apple hardware (Apple's license forbids virtualizing macOS on
non-Apple hardware, so containers are not an option).

```sh
scripts/build-desktop-macos.sh                  # host-arch default
scripts/build-desktop-macos.sh --target x86_64-apple-darwin
scripts/build-desktop-macos.sh --universal      # universal2 binary

# With updater signing (same env vars as Linux):
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/sovereign-updater.key)" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... \
    scripts/build-desktop-macos.sh
```

The script auto-installs Homebrew deps (`tesseract`, `lld`, `protobuf`,
`cmake`), ensures `SDKROOT` is set per `[[feedback_macos_sdkroot_for_bindgen]]`,
stages tesseract into `binaries/`, fetches PDFium + tessdata, and runs
`cargo tauri build` against the release config. Outputs at
`target/<triple>/release/bundle/`. (`protobuf` → `protoc` for
lance-encoding; `cmake` for llama.cpp's Metal backend.)

#### Sharing a local build with friends (no GitHub Actions needed)

This is the route when you're out of CI minutes or just want a build for
a few people. On an Apple-Silicon Mac, `scripts/build-desktop-macos.sh`
(no args) produces `target/aarch64-apple-darwin/release/bundle/dmg/
Sovereign_<ver>_aarch64.dmg`. Hand that `.dmg` over.

The release config sets `bundle.macOS.signingIdentity = "-"`, so the
build is **deep ad-hoc code-signed** during bundling. Ad-hoc signing is
what makes the app *and its nested binaries* (the embedded daemon,
`tesseract`, `pdfium`) runnable on Apple Silicon at all — arm64 refuses
to exec unsigned binaries. It is **not** notarization, so recipients
still see a Gatekeeper warning on first launch. Tell them to either:

- right-click the app → **Open** → **Open**, or
- run once: `xattr -dr com.apple.quarantine /Applications/Sovereign.app`

A real Apple Developer ID + notarization is what removes that prompt
entirely — see "Code signing — Phase 2" below. Updater `.sig` sidecars
(the `TAURI_SIGNING_PRIVATE_KEY` path) are unrelated to launching the app
and aren't needed for hand-shared builds.

Intel-Mac friends: a plain run is arm64-only. Covering x86_64 from an
Apple-Silicon Mac needs the same cross-compile + x86 `tesseract` work as
CI (`--universal`); not wired up for the local script yet.

### Windows

Not containerizable locally on Fedora/macOS — Tauri's MSI bundler is
Windows-only. Push the tag and let GitHub Actions handle Windows
specifically (the `windows-2022` matrix step is unrelated to the
Linux/macOS Vulkan/Metal mess, so it tends to be reliable).

### Dev iteration WITHOUT the release-config overlay

For `cargo tauri dev` rather than `cargo tauri build`, set the env
vars from `binaries/README.md` and run `cargo tauri dev` as usual —
the dev path uses the base `tauri.conf.json` which has no `externalBin`
entries, so it works without staging binaries.

---

## Troubleshooting

### `resource path 'binaries/...' doesn't exist`

You're running `cargo tauri build` with the release overlay before
staging the bundled assets. The release config's `resources` require
`binaries/pdfium/*` and `binaries/paddle-ocr/ppocr-en-v4v5/*` to exist
— stage them per "External binaries — Staging the binaries".

If you're trying to do a plain dev build, use `cargo check` against
the base `tauri.conf.json` (not the release overlay) — the base
config has no `resources` and won't error.

### macOS DMG won't open: "Sovereign is damaged and can't be opened"

The classic Gatekeeper message. v1 builds are ad-hoc signed (so they run
on Apple Silicon) but NOT notarized, so Gatekeeper still warns. Workaround
(use `-r` for the whole bundle):

```sh
xattr -dr com.apple.quarantine /Applications/Sovereign.app
```

…or right-click the app → Open → Open. This is what users hit until
Phase 2 (Developer ID + notarization). Document it in the release notes'
install instructions / your message to friends.

### Windows SmartScreen blocks the installer

Click "More info" → "Run anyway". This will improve once we have an
EV cert (Phase 2).

### AppImage on Linux: "AppImages require FUSE to run"

`sudo apt install libfuse2` on the user's machine. Modern distros have
moved to FUSE 3; AppImage still uses 2.

### Linux build fails: `Could not find dependency: libggml-base.so.0`

You'll only see this if `dynamic-link` has been re-enabled on
`llama-cpp-4`. By default the project **static-links** ggml/llama (the
vendored `llama-cpp-4` drops `dynamic-link` from its `default`
features), so the binary has no `libggml*.so` / `libllama*.so`
`DT_NEEDED` entries at all — every installer (`.deb`, `.rpm`, AppImage)
is self-contained, and `linuxdeploy`'s dependency walk only sees
packageable system libs (`libvulkan.so.1`, GTK, …).

If you turn `dynamic-link` back on, `llama-cpp-sys-4` emits shared libs
(`libggml-base.so.0` … `libmtmd.so.0`) into its hashed CMake-cache dir
(`target/<triple>/llama-cmake-cache/<hash>/{build/bin,lib}/`), which is
on no standard search path — so `linuxdeploy` can't find them and
aborts. To make that path work you'd need to `cargo tauri build
--no-bundle` first, then put that dir on `LD_LIBRARY_PATH` before the
bundling pass (linuxdeploy then copies them into `AppDir/usr/lib/` and
patches the binary RUNPATH to `$ORIGIN/../lib`) — **and** separately
co-package them into the `.deb`/`.rpm` (those bundlers skip the
dependency walk, so they'd build but not launch). Static-linking
sidesteps all of this; prefer it.

Verify a build is self-contained (no ggml/llama in the dynamic deps):

```sh
readelf -d target/<triple>/release/sovereign-desktop | grep -E 'NEEDED.*(ggml|llama|mtmd)'
# → no output when static-linked
```

### deb/rpm installs but won't launch: `libvulkan.so.1: cannot open`

The Vulkan backend still dynamically links the **system** loader
`libvulkan.so.1` (only ggml/llama are static). Tauri doesn't auto-detect
it, so `tauri.release.conf.json` declares it explicitly:
`bundle.linux.deb.depends = ["libvulkan1"]` and
`bundle.linux.rpm.depends = ["vulkan-loader"]`. apt/dnf then pull the
loader on install. The AppImage bundles `libvulkan.so.1` directly, so
it's unaffected. (The GTK/webkit deps are auto-added: deb gets
`libwebkit2gtk-4.1-0`/`libgtk-3-0`/`libappindicator3-1`, rpm auto-detects
the sonames.)

> **Inspecting a `.deb` on Fedora:** `dpkg-deb` is absent, so it silently
> reads nothing. Use `ar x foo.deb && tar -xf control.tar.* && cat control`
> to see the real `Depends`.

### Linux build fails: `'spv' has not been declared` / `glslc` not found

Vulkan build tooling missing. The Linux backend forces
`-DGGML_VULKAN=ON`, so the build needs `glslc` (from `shaderc`), the
SPIR-V headers (`spirv-headers` — ggml-vulkan.cpp `#include`s
`<spirv/unified1/spirv.hpp>` for the `spv::` namespace), and
`libvulkan-dev`. Stock Ubuntu 22.04 has none of `glslc`/`shaderc`/
`spirv-headers`, so both the Containerfile and the CI Linux apt step
register **LunarG's Vulkan SDK apt repo** and install
`libvulkan-dev vulkan-headers spirv-headers spirv-tools shaderc`.

### Build fails: ``Could not find `protoc` `` (in `lance-encoding`)

`lance-encoding`'s build script runs `prost-build`, which shells out to
`protoc`. **None** of the GitHub runner images preinstall it (a slimming
change — don't assume the ubuntu runner has it). Installs, per platform:
- Linux: `apt-get install protobuf-compiler libprotobuf-dev` (the `-dev`
  provides the well-known protos, e.g. `google/protobuf/empty.proto`).
- macOS: `brew install protobuf`.
- Windows: `choco install protoc` (its shim is already on PATH; the
  brew/choco packages bundle the well-known protos).

These are wired into the workflow + `build-desktop-{linux,macos}.sh`.

### macOS build fails: `'path'/'string' is unavailable: introduced in macOS 10.15`

In `llama-cpp-sys-4`'s ggml compile (`ggml-backend-dl.cpp` uses
`std::filesystem`). The `cc` crate picks the deployment target as: (1)
`MACOSX_DEPLOYMENT_TARGET` env if set, else (2) **the Xcode SDK's
`DefaultDeploymentTarget` — 10.13 on Xcode 15.4**, else (3) arch default.
With the env var unset it falls to 10.13, which predates `std::filesystem`.
Fix (in the repo): root `.cargo/config.toml` sets
`[env] MACOSX_DEPLOYMENT_TARGET = { value = "10.15", force = true }`. The
**`force = true` is load-bearing** — a plain string value is skipped by
cargo if the variable is already present/empty in the environment, which
is why earlier env pins (`$GITHUB_ENV`, non-forced `[env]`) were ignored.
The CI workflow also sets it on the Tauri-build step's `env:` as a belt.
NB: this is NOT driven by Tauri's `minimumSystemVersion` — `tauri-build`'s
`cargo:rustc-env` only reaches `sovereign-desktop`'s own rustc and runs
*after* `llama-cpp-sys-4` is already built. It's a COMPILE-time floor, so
a rustflags `link-arg` cannot fix it.

### macOS build fails: `libomp not found` / `OpenMP not found`

The vendored `llama-cpp-4`'s default features include `openmp`, which on
macOS makes `llama-cpp-sys-4` build ggml with `-DGGML_OPENMP=ON` and
demand a Homebrew `libomp` → `libomp.dylib`, a non-self-contained runtime
dep the `.app` would have to bundle (the same trap static-linking closed).
Fix: `sovereign-inference/Cargo.toml`'s macOS `llama-cpp-4` dep uses
`default-features = false, features = ["metal", "mtmd"]` (drops openmp,
keeps mtmd). Linux keeps openmp — `libgomp` is a standard system lib
there. ggml's pthread threadpool covers CPU ops; Metal does the real work.

### Windows build fails: `unresolved import std::os::fd` / `pdfium.dll not found in archive`

Two separate Windows-portability fixes: (1) `sovereign-tools`'
`extract_stage.rs` `StdoutSilencer` used Unix-only `std::os::fd` +
`libc::dup2` + `/dev/null` — now `#[cfg(unix)]` with a `#[cfg(not(unix))]`
no-op stub. (2) `fetch-desktop-binaries.sh` looked only in the PDFium
archive's `lib/`, but bblanchon's `win-x64` archive puts the runtime DLL
in `bin/pdfium.dll` (`lib/` holds only the import lib) — now searches both.

### OCR not available on the bundled installer

OCR now runs on bundled PaddleOCR models — no system install needed.
If the OCR button is hidden, the bundle is missing the models or
pdfium (boot log shows `PaddleOCR models not found … falling back to
tesseract`, then `OCR not available` if no system tesseract either).
Confirm `Contents/Resources/binaries/{paddle-ocr/ppocr-en-v4v5,pdfium}`
exist in the `.app`; if not, the release build skipped the `resources`
overlay (`--config src-tauri/tauri.release.conf.json`) or the assets
weren't staged. The legacy tesseract fallback still works if a user has
system tesseract on PATH, but that's no longer the intended path.

…and re-launches. Phase 2 will eliminate this requirement.

### A platform's job fails but the rest succeed

Re-run only that job from the GitHub Actions UI. Fix the root cause in
a follow-up PR; don't bump the version for a CI-only flake.

### Universal binary on macOS

Currently per-arch. To switch to a universal binary: drop the
`macos-13` runner, add `--target universal-apple-darwin` and remove
the per-arch matrix entries on macOS. Bigger DMG, simpler matrix.

---

## What's NOT covered here

- **CLI / daemon releases**: separate flow, same pattern, separate doc
  (TBD when needed).
- **Internal pre-release dogfooding builds**: the CI workflow only
  fires on `desktop-v*` tags. For nightly-style internal builds, use
  `workflow_dispatch` from the Actions UI.
- **Roll-back**: if a release is bad, mark the GitHub Release as
  "pre-release" or delete it. Anyone who already downloaded the bad
  build needs a manual update notification — there's no auto-update
  channel until Phase 3.

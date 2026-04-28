# Sovereign Desktop — Releasing

The release process for the Sovereign desktop app: macOS (arm64 +
x86_64), Linux (x86_64), Windows (x86_64). Cuts a draft GitHub Release
with installers for all four targets. Unsigned for v1; code signing
and auto-updates are deferred (see "Phase 2" / "Phase 3" below).

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

Semantic versioning. The desktop app's version lives in three places
that must move together; bump them all in a single commit:

| File | Field |
|------|-------|
| `sovereign/crates/sovereign-desktop/src-tauri/Cargo.toml` | `package.version` |
| `sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json` | `version` |
| `sovereign/crates/sovereign-desktop/package.json` | `version` |

A pre-flight script verifying these match is a good follow-up; not in
v1.

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
- [ ] Manual smoke test on your dev machine: launch the app, drop a
      folder, confirm ingest completes, ask one question, confirm
      response. Five-minute test.
- [ ] If you touched OCR: run a folder containing a real scanned PDF
      with `SOVEREIGN_TESSERACT_BIN` etc. set, confirm the offer
      surfaces and works end-to-end.
- [ ] Versions bumped (see above).
- [ ] `CHANGELOG.md` entry written.

### 2. Cut the release

```sh
git commit -am "chore(desktop): release v0.1.0"
git tag desktop-v0.1.0
git push origin main desktop-v0.1.0
```

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

The desktop bundles three external binaries for the OCR pipeline.
They're gated by `lc_ocr_available` — if any are missing the OCR
button hides itself and ingest is unaffected.

| Binary | Purpose | Source |
|--------|---------|--------|
| `tesseract` | OCR engine for scanned PDFs | Platform-installed (v1); static-build hosted on `sovereign-dev` (Phase 2) |
| `libpdfium.dylib` / `pdfium.dll` / `libpdfium.so` | PDF rasterization to images | `bblanchon/pdfium-binaries` releases |
| `tessdata/eng.traineddata` | English language pack | `tesseract-ocr/tessdata` |

### `scripts/fetch-desktop-binaries.sh`

The single source of truth for staging external binaries. Idempotent;
re-run it any time. CI calls it from
`.github/workflows/desktop-release.yml`.

```sh
# auto-detect host triple from rustc
scripts/fetch-desktop-binaries.sh

# or pass an explicit triple (this is what CI does)
scripts/fetch-desktop-binaries.sh aarch64-apple-darwin
```

Output lands at
`sovereign/crates/sovereign-desktop/src-tauri/binaries/`.

### Tesseract — the awkward dependency

In v1 we don't ship a self-contained Tesseract for macOS / Linux. The
brew/apt builds are dynamically linked to `libleptonica`, `libtiff`,
`libjpeg`, `libpng` — copying just the `tesseract` binary out gives
you a 80 KB executable that breaks the moment a user without those
dylibs runs the app.

In CI:
- macOS runners: `brew install tesseract`, copy the binary into place.
  The bundled DMG ships ONLY the brew-linked binary; users without
  `brew install tesseract` themselves will see OCR unavailable. This
  is acceptable for the technical-early-adopter audience.
- Linux runners: `apt install tesseract-ocr`, ditto.
- Windows runners: GitHub's image already includes Tesseract; we copy
  it into place. UB Mannheim's static-ish binary works portably.

**Phase 2** (tracked, not yet built): produce static Tesseract builds
for macOS arm64 / x86_64 and Linux x86_64 once and host them on a
dedicated `sovereign-dev/tesseract-static-binaries` GitHub release.
`fetch-desktop-binaries.sh` then downloads them like it does PDFium.

Until that lands, the macOS/Linux installers' OCR feature requires the
end user to have system tesseract installed. Document this in the
release notes for any version that ships OCR.

### PDFium

`bblanchon/pdfium-binaries` publishes per-platform tarballs. The fetch
script downloads the latest, extracts `lib/<lib_name>`, drops it under
`src-tauri/binaries/pdfium/`. Stable since 2018, no auth required.

If a future PDFium ABI change breaks `pdfium-render`, pin the URL to
a specific release tag in the script.

### tessdata

`tesseract-ocr/tessdata/main/eng.traineddata`, ~30 MB, English only.
For multi-language OCR add language packs to the same dir; the desktop
ships English only in v1.

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

Linux runners need: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`,
`librsvg2-dev`, `patchelf`, `libfuse2`, `tesseract-ocr`. The workflow
installs these.

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
adds the OCR `externalBin` and `resources`. Only release builds use
it; CI invokes Tauri with this flag.

This split is the cleanest available pattern; alternatives (sed-edit
the main config in CI, environment-variable gates, etc.) all add
moving parts.

---

## Code signing — Phase 2 (deferred)

When ready to invest:

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

## Auto-updates — Phase 3 (deferred)

Tauri ships `tauri-plugin-updater`. When ready:

1. Generate an updater key pair (`cargo tauri signer generate`).
2. Add the public key to `tauri.conf.json` `plugins.updater.pubkey`.
3. CI: sign each release artifact with the private key, upload the
   `.sig` files alongside.
4. Host an updater manifest at a stable URL pointing at the latest
   release artifacts and signatures. GitHub Releases works as the
   backing store; the manifest can be a static file at
   `sovereign.dev/updater/desktop-latest.json`.
5. Add `tauri-plugin-updater` as a desktop dep, wire the in-app
   "Check for updates" affordance.

The private key MUST live in repo secrets and never on a developer
laptop. Lose it and you can't ship updates to existing installs without
forcing a fresh download.

---

## Local rehearsal

To run the same build the workflow will run, locally:

```sh
# 1. Stage binaries for your host
scripts/fetch-desktop-binaries.sh

# 2. Make sure you have system tesseract for v1
brew install tesseract            # macOS
sudo apt install tesseract-ocr    # Linux x86_64

# 3. Copy tesseract into place (CI does this — see workflow steps)
HOST="$(rustc -vV | awk '/^host:/ { print $2 }')"
cp "$(brew --prefix tesseract)/bin/tesseract" \
   sovereign/crates/sovereign-desktop/src-tauri/binaries/tesseract-${HOST}

# 4. Build
cd sovereign/crates/sovereign-desktop
cargo tauri build --config src-tauri/tauri.release.conf.json

# 5. Inspect bundles
ls -la ../../../target/${HOST}/release/bundle/*/
```

For dev iteration WITHOUT the release-config overlay, set the env
vars from `binaries/README.md` and run `cargo tauri dev` as usual.

---

## Troubleshooting

### `resource path 'binaries/tesseract-...' doesn't exist`

You're running `cargo tauri build` (or just `cargo check` with the
release config) without first running `scripts/fetch-desktop-binaries.sh`.
The release config requires the binaries to be present.

If you're trying to do a plain dev build, use `cargo check` against
the base `tauri.conf.json` (not the release overlay) — the base
config has no `externalBin` and won't error.

### macOS DMG won't open: "Sovereign is damaged and can't be opened"

The classic Gatekeeper-on-unsigned-app message. Workaround:

```sh
xattr -d com.apple.quarantine /Applications/Sovereign.app
```

This is what users will hit on unsigned v1 builds. Consider documenting
this in the release notes' install instructions.

### Windows SmartScreen blocks the installer

Click "More info" → "Run anyway". This will improve once we have an
EV cert (Phase 2).

### AppImage on Linux: "AppImages require FUSE to run"

`sudo apt install libfuse2` on the user's machine. Modern distros have
moved to FUSE 3; AppImage still uses 2.

### Tesseract OCR not available on the bundled installer

System tesseract not installed. v1 builds depend on it (see "External
binaries — Tesseract — the awkward dependency"). User runs:

```sh
brew install tesseract           # macOS
sudo apt install tesseract-ocr   # Linux
```

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

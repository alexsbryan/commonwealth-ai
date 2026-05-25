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
- [ ] If you touched OCR: run a folder containing a real scanned PDF
      with `SOVEREIGN_TESSERACT_BIN` etc. set, confirm the offer
      surfaces and works end-to-end.
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

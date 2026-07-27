# Releasing Commonwealth AI

This repo ships **two** user-facing artifacts from **two independent**
tag-triggered GitHub Actions workflows. They do not bundle — tagging one does
not build the other.

| Artifact | Tag prefix | Workflow | Output | How users get it |
|---|---|---|---|---|
| **CLI** (`sovereign`) | `cli-v*` | `.github/workflows/cli-release.yml` | `sovereign-<target>.tar.gz` (3 binaries) + `SHA256SUMS` | `curl -fsSL https://svrnme.sh/install.sh \| sh` |
| **Desktop app** | `desktop-v*` | `.github/workflows/desktop-release.yml` | `.dmg` / `.AppImage` / `.deb` (/ `.msi`) | download + double-click |

Each workflow creates its **own draft GitHub Release** keyed to its tag, so
`cli-v0.1.18` and `desktop-v0.1.18` are separate release pages with separate
assets. Different tag prefixes mean they never fire together.

The CLI tarball contains all three product binaries the dispatcher needs —
`sovereign-cli` (the `sovereign` dispatcher), `sovereign-cli-daemon`, and
`sovereign-cli-llm`. There is **no separate daemon release**; the daemon ships
inside the CLI tarball.

---

## Versioning — one workspace version for both

There is a single repo-wide version: `[workspace.package].version` in the root
`Cargo.toml` (every crate inherits it via `version.workspace = true`). Both tag
prefixes should carry that same number, cut from the same commit — e.g. release
`0.1.18` as **both** `cli-v0.1.18` and `desktop-v0.1.18`.

Bump it with the desktop script (it also moves the desktop's `tauri.conf.json`
+ `package.json`, which must agree, and verifies all three):

```sh
scripts/bump-desktop-version.sh 0.2.0     # or: patch | minor | major
```

Both release workflows **verify the tag matches the workspace version** before
spending a build (`check-desktop-version.sh` for desktop; an inline step in
`cli-release.yml`), so a mistyped tag fails in seconds, not after a long build.

---

## The one-button path (recommended)

**Actions → Release (manual) → Run workflow.** Pick what to ship (`both` /
`cli` / `desktop`) and leave *version* blank to use the current workspace
version. It's owner-only (repository owner), verifies the version against
`Cargo.toml`, then **reuses the two pipelines below** (via `workflow_call`) to
produce the same **draft** releases — which you still smoke-test and **Publish**
by hand.

Bump + commit the version first (above) and let CI go green on `main`; the
workflow refuses to build if the version input disagrees with `Cargo.toml`.

Everything below documents the underlying tag-triggered pipelines the button
drives. Reach for the manual `git tag` path when you want to release a specific
past commit rather than current `main`, or a single artifact out of band.

---

## Releasing the CLI

### 1. Pre-flight

- [ ] `./scripts/sovereign-test.sh --human` green (the full workspace gate).
- [ ] Version bumped (above) and committed.

### 2. Tag + push

```sh
v="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"   # e.g. 0.1.18
git tag "cli-v$v"
git push origin main "cli-v$v"
```

The tag push triggers `cli-release.yml`. It builds the three binaries per
platform (`cargo build --release -p sovereign-cli -p sovereign-cli-daemon
-p sovereign-cli-llm`), packages `sovereign-<target>.tar.gz` + a per-file
`.sha256`, and uploads them to a **draft** release named `cli-v$v` along with a
combined `SHA256SUMS`.

**Platform matrix:** Linux x86_64 and **native macOS Intel (x86_64)** are
enabled on a tag. Intel builds on the **self-hosted Intel-Mac runner** (labels
`self-hosted, macOS, X64`) — the same box + runner the desktop release uses, so
**start `./run.sh` before tagging** (manual-start). Runner setup lives in the
desktop [`RELEASING.md`](sovereign/crates/sovereign-desktop/RELEASING.md) under
*"macOS Intel (x86_64) — self-hosted runner"*. The `release` job is decoupled
(`if: !cancelled()`), so a down runner can't block the Linux tarball. macOS
arm64 is ready to uncomment in `cli-release.yml` once a local
`cargo build --release --target aarch64-apple-darwin` of the three binaries is
validated. (GitHub's hosted Intel `macos-13` runner stays avoided — it hangs on
"Waiting for a runner" and drains the Actions budget.)

**No CI / no runner?** All three CLI targets (mac aarch64, mac x86_64 cross,
Linux x86_64 via podman) can be built, packaged, and uploaded from one arm64
Mac with `scripts/release-cli-local.sh` — CI-identical packaging (same
binaries, tar layout, `.sha256` sidecars) plus a `SHA256SUMS` regeneration
that keeps any CI-built assets on the draft covered. Same `--skip-*` /
`--no-upload` / `--upload-only` flags as `release-desktop-local.sh`; the
desktop runbook's one-time setup table applies (podman machine, gh auth).

### 3. Promote

1. Watch `https://github.com/<owner>/<repo>/actions/workflows/cli-release.yml`.
2. When green, a draft release appears with `sovereign-<target>.tar.gz`,
   its `.sha256`, and `SHA256SUMS`.
3. Smoke it before publishing — download the tarball for your platform, verify
   the checksum, extract, and run `./sovereign-cli --version` (it should report
   the workspace version, matching the tag).
4. Click **Publish release** in the GitHub UI.

### 4. The installer

`landing/install.sh` (served at `https://svrnme.sh/install.sh`) is the
`curl | sh` target. On `latest` it resolves the **newest `cli-v*` release** via
the GitHub API:

```sh
curl -fsSL "https://api.github.com/repos/<owner>/<repo>/releases" \
  | grep '"tag_name"' | grep -oE 'cli-v[0-9][^"]*' | head -n1
```

This is deliberate: GitHub's `/releases/latest` is a single repo-global pointer
**shared with the `desktop-v*` stream**, so it can resolve to a desktop release
that has no CLI tarball. Resolving `cli-v*` by name keeps the one-liner correct
regardless of which stream published most recently. The unauthenticated
`/releases` list excludes drafts, so the installer only ever picks a *published*
CLI release. To pin a specific version: `SOVEREIGN_VERSION=cli-v0.1.18`.

The installer downloads the three binaries into `~/.local/bin` (override with
`SVRNMESH_INSTALL_DIR`), symlinks `sovereign` → `sovereign-cli`, verifies the
checksum against `SHA256SUMS`, and prints `svrn setup` as the next step.

---

## Releasing the desktop app

Same shape (bump → tag `desktop-v$v` → CI → promote draft), but with more
moving parts: a four-platform Tauri matrix, bundled OCR/PDFium assets, ad-hoc
code signing, and the auto-updater keypair. **The authoritative checklist is
[`sovereign/crates/sovereign-desktop/RELEASING.md`](./sovereign/crates/sovereign-desktop/RELEASING.md)** —
follow it for desktop releases. The one-line version:

```sh
scripts/bump-desktop-version.sh 0.2.0
git commit -am "chore(desktop): release v0.2.0"
git tag desktop-v0.2.0 && git push origin main desktop-v0.2.0
# → desktop-release.yml → promote the draft after smoke-testing an installer
```

No CI (or no Intel runner)? The whole four-platform matrix — macOS both
arches, Linux, and Windows (containerized cargo-xwin + NSIS) — can be built
and uploaded from one arm64 Mac with `scripts/release-desktop-local.sh` —
see "Full local release from the arm64 Mac" in the desktop RELEASING.md.

### After promoting: confirm the payload, not just the filename

**A valid signature does not mean you shipped the right build.** The `.sig`
signs the archive's BYTES; it says nothing about which version those bytes
contain. An artifact named `svrnmesh_<new>_<arch>.app.tar.gz` that actually
contains the *previous* build verifies perfectly, installs happily, and puts
every user who updates into a permanent update loop — they land back on the
old version, the manifest endpoint keeps offering the new one, forever.

That shipped once (desktop-v0.3.5, 2026-07-27): `target/` isn't cleaned
between releases, `cargo tauri build` dies in the DMG cosmetic step *before*
emitting updater artifacts, so the previous version's archive was still on
disk — and the arch-qualify step stamped the new version's name onto it.

The build and release scripts now assert this themselves and abort on a
mismatch. To confirm a published release by hand:

```sh
v=0.3.6   # the version you just published
curl -sL -o /tmp/a.tar.gz \
  "https://github.com/alexsbryan/svrnmesh-releases/releases/download/desktop-v$v/svrnmesh_${v}_aarch64.app.tar.gz"
tar -xzOf /tmp/a.tar.gz '*.app/Contents/Info.plist' \
  | plutil -extract CFBundleShortVersionString raw -o - -    # MUST print $v
```

If it prints anything else, unpublish immediately — clients that already
updated are stuck until a correct build replaces it.

---

## How the two coexist on GitHub Releases

Both workflows publish **drafts** (a tag push *stages* a release; you publish it
manually after smoke-testing). Once published, GitHub's global "latest release"
pointer flips to whichever stream published most recently — which is exactly why
the CLI installer resolves `cli-v*` by name rather than trusting `latest`. The
desktop auto-updater similarly queries for the latest `desktop-v*` tag (via the
`svrnme.sh` manifest endpoint), so neither consumer depends on the shared
`latest` pointer.

To cut a coordinated release of both at version `X.Y.Z`: bump once, commit, then
push both tags (`cli-vX.Y.Z` and `desktop-vX.Y.Z`) — two workflows run in
parallel and produce two independent draft releases.

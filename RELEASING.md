# Releasing Commonwealth AI

This repo ships **three** user-facing artifacts. The two Rust ones come from
**independent** tag-triggered GitHub Actions workflows; the VS Code extension
has no CI pipeline and releases from a local script. Nothing bundles — tagging
one does not build the others.

| Artifact | Tag prefix | Built by | Output | How users get it |
|---|---|---|---|---|
| **CLI** (`sovereign`) | `cli-v*` | `.github/workflows/cli-release.yml` | `sovereign-<target>.tar.gz` (3 binaries) + `SHA256SUMS` | `curl -fsSL https://svrnme.sh/install.sh \| sh` |
| **Desktop app** | `desktop-v*` | `.github/workflows/desktop-release.yml` | `.dmg` / `.AppImage` / `.deb` (/ `.msi`) | download + double-click |
| **VS Code extension** (`svrn fim`) | `vscode-v*` | `scripts/release-vsix-local.sh` | `sovereign-fim-<ver>.vsix` + `.sha256` | `svrn setup --fim`, or install the `.vsix` by hand |

Each release is its **own** GitHub Release keyed to its tag, so `cli-v0.1.18`
and `desktop-v0.1.18` are separate release pages with separate assets.
Different tag prefixes mean they never fire together.

**All three publish to the public shelf repo** (`alexsbryan/svrnmesh-releases`),
not to this source repo — assets on a private repo aren't anonymously
fetchable, which would break `install.sh`, the landing-page downloads, and the
desktop auto-updater. Override with `RELEASES_REPO` when testing.

**The tag prefix is load-bearing, and every consumer must filter on its own.**
`landing/install.sh` takes the max-semver `cli-v*`; `landing/api/desktop/*.js`
take the max-semver non-draft `desktop-v*`. Neither uses GitHub's
`/releases/latest`, which is a single repo-global pointer shared by all three
streams. A new artifact stream on this shelf **must** be invisible to the
existing resolvers — verify that before publishing, and publish with
`--latest=false` so the shelf's "Latest" badge keeps pointing humans at the
desktop app rather than following whatever shipped most recently.

The CLI tarball contains all three product binaries the dispatcher needs —
`sovereign-cli` (the `sovereign` dispatcher), `sovereign-cli-daemon`, and
`sovereign-cli-llm`. There is **no separate daemon release**; the daemon ships
inside the CLI tarball.

---

## Versioning — one workspace version for the two Rust artifacts

(The VS Code extension is exempt — it carries its own version in
`packages/vscode-sovereign/package.json`. See its section below.)

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

## Which host can cut which legs

The local drivers (`scripts/release-all.sh` and the two per-artifact scripts
under it) run on **two** hosts, and each builds a different subset. Capability
is decided once in `scripts/lib/release-host.sh`; nothing re-derives it from
`uname`.

| Leg | arm64 Mac | x86_64 Linux |
|---|---|---|
| CLI `aarch64-apple-darwin` | yes | **no** — needs the macOS SDK |
| CLI `x86_64-apple-darwin` | yes | **no** — needs the macOS SDK |
| CLI `x86_64-unknown-linux-gnu` | yes (qemu) | yes — **native** |
| Desktop macOS arm64 + Intel (`.dmg`, `.app.tar.gz`) | yes | **no** — needs `codesign`/`hdiutil`/`plutil` |
| Desktop Linux (`.AppImage`, `.deb`, `.rpm`) | yes (qemu) | yes — **native** |
| Desktop Windows (`.exe`, cargo-xwin) | yes | yes |

A host **announces** the legs it cannot build and carries on with the rest; it
never skips one silently. Two things follow:

- **The Linux legs are faster on Linux.** On the Mac they run
  `--platform linux/amd64` under qemu, where ggml-vulkan's shader compile
  deadlocks unless it is pinned to a single core — that pin is why the leg is
  slow. Natively there is no emulation and no cap, so the cap is keyed to the
  emulation rather than to the platform string.
- **The Windows leg needs a case-sensitivity repair on Linux, and it is not
  optional.** `xwin` splats each MSVC import library under two spellings only —
  the real lowercase file and an all-caps symlink — while a crate emitting
  `cargo:rustc-link-lib=DirectML` makes `lld-link` ask for the canonical
  mixed-case `DirectML.lib`. macOS never notices because APFS is
  case-insensitive; on Linux the leg dies at link with
  `could not open 'DirectML.lib'`. 70 of the 453 import libraries lack their
  canonical spelling, so which ones bite is decided by the dependency graph, not
  by a fixed list. `build-entrypoint-windows.sh` therefore DERIVES the aliases
  from the SDK headers, which do preserve canonical case (139 symlinks on the
  current SDK). If you see a fresh `could not open 'SomeLib.lib'` after a
  dependency change, that pass is where it belongs — do not add a one-off
  symlink. This is why the table above reads "yes" for Windows on Linux: it did
  not before 2026-08-10, when this leg was run on Linux for the first time.
- **A release can be cut from both machines.** Both push into the same
  `cli-v*` / `desktop-v*` drafts. The CLI's provenance gate refuses any
  tarball whose `.buildinfo` sidecar does not match the version *and commit*
  being released, so the two halves cannot disagree; and `release-all.sh
  --publish` counts assets **on the release** (≥4 CLI, ≥12 desktop), so the
  draft cannot be flipped public until both halves have landed. Build the
  Apple legs on the Mac, the rest wherever you like, and publish from
  whichever machine finishes second.

  **One prerequisite that is not about tooling: any host cutting a DESKTOP leg
  needs the updater signing secret**, and it is not enough to have the key
  file. `~/.tauri/sovereign-updater.key` is an *rsign encrypted* secret key, so
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` is required alongside it. On the Mac the
  password lives in `~/.zshrc` — which is why `release-desktop-local.sh`'s own
  error says so — and a second machine has nothing until you put it there too
  (`~/.bashrc` on the Fedora host). Without it the driver hard-fails, or with
  `--no-upload` warns and produces bundles with no `.sig`: not shippable, and
  auto-update breaks for those platforms. Keep the key file at mode `0600`.
  The CLI legs need no signing secret at all, which is why RuggedFox could cut
  and verify the Linux CLI tarball before any of this was set up.

On the Fedora workstation the release straddles the toolbox boundary, and
**three** tools sit on different sides of it:

| Tool | Where it lives | Needed for |
|---|---|---|
| `podman` | **host only** | every container leg |
| `cargo` + native build deps | **toolbox only** (`sovereign-vulkan`) | the workspace test gate, the router-embed cache check |
| `gh` | **host** — but not installed there by default | the entire preflight, every upload, the publish gate |

Run the drivers **from the host**. The drivers resolve the cargo hop themselves
and print which route they took (`native cargo: toolbox run -c
sovereign-vulkan`); override with `RELEASE_NATIVE_RUN_PREFIX` /
`SOVEREIGN_TOOLBOX`.

`gh` is the one that bites, because the toolbox ships it and the Fedora host
does not — so `release-all.sh` dies on its first preflight line (`'gh' is
required on PATH`) from the host, while every container leg dies from inside the
toolbox. Neither side can cut a release until you `sudo dnf install gh` on the
host. Auth needs no repeat: the token lives in the login keyring and both sides
read the same `~/.config/gh/hosts.yml`, so a host install inherits the
toolbox's session.

The drivers' own negative controls — including the host-capability split and the
build-context budget — are `scripts/tests/run-all.sh`, which
`scripts/pre-push.sh` runs whenever a release driver, a Containerfile, or
`.containerignore` changes.

One thing to keep in mind when adding to the tree: `.containerignore` decides
what podman streams into every container build, and an un-ignored bulky
directory does not fail anything — it just adds a multi-minute stall to every
build, silently. It was 36 GB on RuggedFox before `research/` and `models/` were
excluded, for an image build whose only `COPY` is a single entrypoint script.
`scripts/tests/release-build-context.sh` budgets the total and names the
offender.

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

> **The tag push does NOT start a build.** `cli-release.yml` and
> `desktop-release.yml` carry only `workflow_dispatch` + `workflow_call`
> triggers — the `on: push: tags:` trigger they once had is gone, so pushing a
> tag and waiting is a silent no-op. Start the build one of three ways:
> **Actions → Release (manual)** (the one-button path above, which tags for
> you), **Actions → the individual workflow → Run workflow** with the tag as
> input, or the fully local `scripts/release-all.sh` (below). Tagging is still
> worth doing first — `release-all.sh` refuses to build unless both tags exist
> and point at HEAD.

Once dispatched, `cli-release.yml` builds the three binaries per
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
# then DISPATCH desktop-release.yml (the tag push alone builds nothing — see
# the callout under "Releasing the CLI § 2") → promote the draft after
# smoke-testing an installer
```

No CI (or no Intel runner)? The whole four-platform matrix — macOS both
arches, Linux, and Windows (containerized cargo-xwin + NSIS) — can be built
and uploaded from one arm64 Mac with `scripts/release-desktop-local.sh` —
see "Full local release from the arm64 Mac" in the desktop RELEASING.md. An
x86_64 Linux host runs the same script and cuts the Linux + Windows legs
natively; see "Which host can cut which legs" above.

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

If it prints anything else, the release needs repair before more clients
update into the loop.

### Repairing a mislabeled updater archive — repackage, do NOT rebuild

The failure is narrow: `cargo tauri build` produced a correct `.app` (the DMG
proves it) and only failed to re-emit the version-less `svrnmesh.app.tar.gz`.
So the *right bits are already published* — inside the DMG. Rebuilding is both
unnecessary (hours) and **wrong**: `main` has usually moved past the release
tag, so a rebuild ships different code under a published version number, which
is exactly what `release-all.sh`'s already-published guard exists to prevent.

Repackage the published bundle instead. This is what fixed desktop-v0.3.5
(2026-07-27), start to finish in minutes:

```sh
v=0.3.5
for arch in aarch64 x64; do
  curl -sL -o "$arch.dmg" \
    "https://github.com/alexsbryan/svrnmesh-releases/releases/download/desktop-v$v/svrnmesh_${v}_${arch}.dmg"
  hdiutil attach "$arch.dmg" -nobrowse -readonly -mountpoint "mnt-$arch"
  ditto "mnt-$arch/svrnmesh.app" "stage-$arch/svrnmesh.app"   # ditto: preserves perms + signature
  hdiutil detach "mnt-$arch"
  ( cd "stage-$arch" && COPYFILE_DISABLE=1 tar -czf "../svrnmesh_${v}_${arch}.app.tar.gz" svrnmesh.app )
  cargo tauri signer sign "svrnmesh_${v}_${arch}.app.tar.gz"   # reads TAURI_SIGNING_PRIVATE_KEY{,_PASSWORD}
done
gh release upload "desktop-v$v" --repo alexsbryan/svrnmesh-releases --clobber \
  svrnmesh_${v}_*.app.tar.gz svrnmesh_${v}_*.app.tar.gz.sig
```

`COPYFILE_DISABLE=1` keeps bsdtar from injecting AppleDouble (`._*`) entries —
Tauri's Rust tar writer emits none, and the archive must match. The only
remaining difference from a Tauri-built archive is trailing slashes on
directory entries; extraction is identical.

Then verify against the **published** URL (not your local copy), and confirm
the signature key ID still matches the one clients already trust — a
mismatched key makes the update silently unacceptable to every existing
install:

```sh
python3 - svrnmesh_${v}_aarch64.app.tar.gz.sig <<'EOF'
import base64, sys
sig = base64.b64decode(open(sys.argv[1]).read()).decode()
print(base64.b64decode(sig.strip().splitlines()[1])[2:10][::-1].hex().upper())
EOF
# must equal the key ID derived from tauri.conf.json's plugins.updater.pubkey
```

Do **not** unpublish as a first move. Unpublishing removes the DMG that is the
only surviving copy of the correct bundle, and strands clients with no release
to move to. Repair in place; unpublish only if the `.app` itself is bad.

---

## Releasing the VS Code extension

`packages/vscode-sovereign` — pure TypeScript bundled by esbuild into one
platform-neutral `.vsix`. No cross-compilation, no containers, no CI pipeline:
`scripts/release-vsix-local.sh` **is** the release path.

**It versions itself.** The extension's version lives in its own
`package.json` and is deliberately *not* pinned to the workspace version — it
ships on its own cadence, so `bump-desktop-version.sh` does not touch it. Bump
it there, then:

```sh
scripts/release-vsix-local.sh              # test + package + smoke + draft
scripts/release-vsix-local.sh --publish    # ...and publish + verify anonymously
scripts/release-vsix-local.sh --no-upload  # build only
```

The script runs `npm test`, packages, then **installs the packaged `.vsix`
into local VS Code before uploading** — a `.vsix` that won't install is the one
failure a checksum cannot catch. With `--publish` it also re-downloads the
published asset with the GitHub token stripped from the environment and checks
the sha256, which is the only real proof that someone who is not you can get it.

Two things the script handles that are easy to get wrong by hand:

- **`LICENSE`.** The manifest declares `AGPL-3.0-or-later`; `vsce` only *warns*
  when the text is missing, so a hand-packaged `.vsix` silently ships a license
  reference with no license. The script copies the repo-root `LICENSE` in.
- **Relative links in `README.md`.** The README is bundled into the `.vsix` and
  rendered in the Extensions pane. `vsce` rewrites relative links against the
  `repository` field — which is the **private** source repo, so every such link
  is a 404 for the people actually reading it. Keep the packaged README
  self-contained; point at `https://svrnme.sh` rather than at repo paths.

Not on the Marketplace yet — this is the sideload shelf. Publishing under a
real `sovereign` publisher ID needs an Azure DevOps PAT and a verified
publisher, which is a beta-time decision.

## How the three coexist on GitHub Releases

All three publish **drafts** first (you publish manually after smoke-testing).
Once published, GitHub's global "latest release" pointer flips to whichever
stream published most recently — which is exactly why the CLI installer
resolves `cli-v*` by name rather than trusting `latest`. The desktop
auto-updater similarly queries for the latest `desktop-v*` tag (via the
`svrnme.sh` manifest endpoint), so no consumer depends on the shared `latest`
pointer. The extension stream publishes with `--latest=false` so the badge
stays on the desktop app.

To cut a coordinated release of both at version `X.Y.Z`: bump once, commit, then
push both tags (`cli-vX.Y.Z` and `desktop-vX.Y.Z`) — two workflows run in
parallel and produce two independent draft releases.

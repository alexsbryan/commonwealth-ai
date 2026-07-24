# CI/CD economy program

**Status:** active as of 2026-07-24.
**Owner:** repo steward.
**Re-measure with:** `scripts/ci-spend-audit.sh --since <YYYY-MM-DD>`

This document explains what gates this repo, what that costs, and why the
split falls where it does. It exists because the previous arrangement failed
in the least visible way possible, and the failure mode is worth naming
before the numbers.

---

## 1. The incident

On 2026-07-24 every GitHub Actions job in this repo began failing after about
four seconds with:

> The job was not started because recent account payments have failed or your
> spending limit needs to be increased.

Nothing was wrong with the code. The month's Actions allowance was gone.

The dangerous part is how that presents. A PR page does not say "this repo
has run out of money." It shows checks that ran and finished, very fast. The
observable symptom was *"dependabot is putting up worthless PRs because the
gates are shot"* — which is true, but the mechanism was not leniency. **There
was no gate at all, and had not been for some time.**

The lesson generalises past this repo: **a metered gate has a failure mode an
unmetered gate does not — it can stop existing without announcing it.** Every
design decision below follows from taking that seriously.

---

## 2. The audit

Measured over 2026-07-01..24, from real per-job wall time via the GitHub jobs
API. (GitHub's own `/timing` endpoint reports `billable.total_ms: 0` on this
repo, so it cannot be trusted; `scripts/ci-spend-audit.sh` computes billed
minutes itself, applying runner multipliers and correctly counting
self-hosted runners as free.)

| Workflow | jobs | billed min | share |
|---|---:|---:|---:|
| CI | 778 | 2,992 | 56.4% |
| Dependabot's own update runs | 58 | 1,017 | 19.2% |
| Desktop release | 43 | 637 | 12.0% |
| docs | 166 | 241 | 4.5% |
| CLI release | 33 | 196 | 3.7% |
| Mesh soak (nightly) | 24 | 122 | 2.3% |
| Labeler / CLA / weekly / baseline-tighten | 97 | 97 | 1.8% |
| **Total** | **1,199** | **5,302** | |

Over 24 days that projects to **~6,600 billed minutes per 30 days against a
3,000-minute private-repo allowance — 2.2× over.**

### The five mechanisms behind that number

Ranked by cost, because the ranking is not what intuition suggests.

**1. The Actions cache was empty. Measured: 0 bytes, 0 entries.**
So every CI run was a cold build of the whole workspace including
llama.cpp/ggml. `Check + Test (workspace)` had a **median of 56.7 minutes**.
The cause was cache-writer contention: every run on every ref called
`Swatinem/rust-cache` with save enabled, putting several multi-GB writers
against GitHub's 10 GB per-repo cache budget, each evicting the others by
LRU. This is a vicious cycle — builds cost 57 minutes because the cache is
cold, the cache stays cold because runs keep getting evicted and failing, and
the 57-minute builds consume the allowance that would have let a run finish
and save.

**2. The C++ cache key was keyed to the wrong thing.**
`llama-cpp-sys-4` builds llama.cpp+ggml from source: ~20 minutes, the single
largest fixed cost in the job. Its cache key was `hashFiles('Cargo.lock')`.
But **`Cargo.lock` changed 29 times in July, while `llama-cpp-sys-4`'s version
has changed 4 times in the repo's entire history.** So 29 dependency bumps —
most of them dependabot's — each threw away a perfectly good C++ tree and paid
20 minutes to rebuild bit-identical output.

**3. Nothing cancelled superseded builds on main.**
`cancel-in-progress` was set for pull requests but deliberately exempted main,
on the grounds that main's runs are the only cache writers. The measured push
cadence is bursty — **seven pushes to main in one four-hour stretch on
2026-07-24** — so that exemption bought six redundant full workspace builds
per burst in order to preserve a cache write the seventh run performs anyway.

**4. Dependabot cost more than it returned.**
Its *update runs* alone — the resolution passes that produce PRs, before any
CI touches them — were **1,017 minutes, 19% of total spend**, because a cargo
resolution against this 40-crate lock takes 32-35 minutes. Each PR it opened
then dragged a ~57-minute workspace compile behind it. Of 23 dependabot PRs
opened, most were **closed rather than merged**. We were paying a premium for
review noise.

**5. Every change was gated twice, and gated regardless of relevance.**
CI fired on both `pull_request` and `push: [main]` (1,461 and 1,531 minutes).
And with no path filtering, a commit touching only Markdown, `docs/`, or
`.claude/` config paid the full workspace compile — in a repo whose history is
full of exactly those commits.

### Two structural facts that shaped the fix

- **`main` had no branch protection at all** (the API returns 404). So even a
  green CI run enforced nothing.
- **`main` is 100% direct pushes — zero merge commits.** Combined with the PR
  authorship split (23 of 37 PRs are dependabot's), this means the entire
  `pull_request` CI lane was being spent almost exclusively on PRs that were
  then thrown away.

---

## 3. The program

Two principles, in priority order.

> **I. The primary gate must be unmetered.**
> Correctness cannot depend on an account balance. The gate that decides
> whether code is fit to leave a machine runs on hardware we already own.
>
> **II. Metered capacity buys what local capacity cannot.**
> A clean-checkout build, and a gate over contributors whose machines we do
> not control. Not a second opinion on work already verified.

### Tier 0 — the local gate (`scripts/pre-push.sh`)

Runs on every `git push`. Costs nothing, so it can run forever.

It scopes to what the push changes — a docs-only push costs about a second —
and reuses your warm `target/`. The workspace suite runs on cargo-nextest
(59s vs 126s for serial `cargo test`).

| Gate | When |
|---|---|
| `cargo fmt --all --check` | Rust files changed |
| `xtask docs-gate` (cited paths resolve) | always — a citation usually breaks because *code* was renamed |
| `scripts/sovereign-test.sh --human` | Rust files changed |
| `npm run check` + `npm run test` | `sovereign-desktop/` changed |

Installed by `scripts/install-git-hooks.sh` (called from `scripts/bootstrap.sh`),
which points `core.hooksPath` at the version-controlled `.githooks/`. Hooks
are therefore reviewed artifacts that update with a `git pull`, not per-clone
files that drift silently between machines.

Escape hatches are real and deliberate — a gate with no escape hatch gets
uninstalled:

```
git push --no-verify                  # skip all hooks, one push
SOVEREIGN_SKIP_PREPUSH=1 git push
SOVEREIGN_PREPUSH_QUICK=1 git push    # fmt + docs only, skip the test run
./scripts/pre-push.sh                 # run the gate by hand, no push
```

**It fails closed.** If `git diff` cannot resolve the push range (unknown sha,
shallow clone, pruned ref) the hook gates *everything* rather than reporting
"no changes" and passing. That bug existed in the first draft and was caught
while testing it — which is the argument for testing gates rather than
trusting them.

### Tier 1 — CI (`.github/workflows/ci.yml`)

Confirms Tier 0 on a clean checkout, and is the real gate for contributions
from machines we do not control.

- **`changes`** — one ~20-second job deciding which gates the diff can break.
  Deliberately *not* workflow-level `paths-ignore`, which would suppress the
  aggregator too and leave a required check permanently "expected."
- **`fmt`** — rustfmt. Seconds, cannot flake. Runs when Rust changed.
- **`desktop`** — svelte-check + vitest. Node only, ~2 min. Runs when the
  desktop crate changed.
- **`test`** — the workspace compile and test suite. This job *is* the CI
  budget; everything else in the file exists to keep it from running
  needlessly or rebuilding what it already built.
- **`ci-ok`** — aggregates the above into one check. **This is the check to
  require in branch protection.** It treats `skipped` as success (so path
  filtering and branch protection stay compatible) and `cancelled` as failure
  (a cancelled run has proven nothing).

The five fixes, against the five mechanisms in §2:

1. **One cache writer.** `save-if: ${{ github.ref == 'refs/heads/main' }}`.
   Main writes, PRs restore read-only. One writer and many readers is the
   configuration in which a cache this size survives.
2. **Cache the C++ on what invalidates it** — `llama-cpp-sys-4`'s version plus
   the toolchain, resolved from `Cargo.lock` at runtime, instead of the lock's
   hash.
3. **`cancel-in-progress` now includes main.** The newest run on a ref still
   writes the cache; only already-obsolete ones are killed.
4. **Path-filtered gates**, so a docs commit cannot buy a workspace compile.
5. **One definition of "the tests pass."** CI runs `scripts/sovereign-test.sh`
   — the same script the pre-push hook and the daemon's `test_status` watcher
   run. This raised coverage as well as cutting cost: CI previously ran a bare
   `cargo test --workspace`, which resolves a *different* feature set than the
   local gate, so **the feature-gated dev-verb suites (`sovereign-cli/dev-tools`)
   had never run in CI at all.** The redundant `cargo check --all-targets`
   pass is gone — it emitted rmeta the test build then re-did with full
   codegen, reusing nothing. Benches, the one target class `cargo test` misses,
   get a cheap incremental check instead.

Also: a 75-minute job timeout (a hung link must not bill open-ended), and
test adapter logs uploaded as an artifact on failure — at ~57 minutes cold,
re-running CI to see *why* it failed is the difference between a diagnosis
and another day of burned allowance.

### Tier 2 — scheduled lanes

Off the critical path entirely; advisory.

- `weekly` — api-surface, features, cycles, timings, doc-coverage, advisories.
- `mesh-soak` — **moved from nightly to Saturdays.** A soak looks for
  slow-accumulating faults; a weekly sample finds those about as well as a
  nightly one at a seventh the cost. It is `continue-on-error: true`, so six
  of every seven results were not being read.
- `baseline-tighten` — Mondays, banks ratchet improvements.

### Dependabot

**Monthly, one grouped PR per ecosystem, majors included**
(`.github/dependabot.yml`), with `open-pull-requests-limit: 1` as a hard
backstop against a second concurrent workspace compile.

Majors previously got individual PRs on the theory that they "deserve
individual review." In practice they were closed unread alongside everything
else, so the split bought nothing and cost an update run each. Reviewing one
monthly batch you actually read beats ignoring five weekly ones.

Security advisories are **not** governed by this file — they live in Settings
→ Code security and keep their own immediate cadence. A CVE should not wait
for the monthly window.

---

## 4. The budget

| Lane | Est. billed min / month |
|---|---:|
| `test` (compile gate) — cancelled + path-filtered + warm cache | ~600–800 |
| `fmt` + `desktop` + `docs-gate` | ~350 |
| `changes` (the filter job itself) | ~130 |
| Dependabot (1 update run + 1 CI run) | ~60 |
| Mesh soak (weekly) + weekly quality | ~50 |
| Labeler + CLA | ~85 |
| Releases (on demand) | ~250 |
| **Total** | **~1,500–1,700** |

Against a 3,000-minute allowance that is roughly 45% headroom — deliberately,
because the number that matters is not the average month but the month with a
release, a dependency crisis, and a contributor.

### Where the remaining risk is

**Release builds are the expensive per-run item.** macOS runners bill at
**10×**: `Desktop release / build / macos-arm64` measured **250 billed minutes
per run** (2 runs, 500 minutes). At two releases a month that is fine; at two a
week it becomes the top line item. If release cadence rises, that job — not
CI — is the thing to attack, and the self-hosted Intel Mac already referenced
by `desktop-release.yml` is the obvious lever, since self-hosted minutes are
free.

**Cold-cache months.** Bumping `rust-toolchain.toml` invalidates the rust
caches by design. Expect the first run after a toolchain bump to cost a full
cold build, and do not schedule one next to a release week.

---

## 5. Operating rules

1. **Re-run the audit monthly.** `scripts/ci-spend-audit.sh --since <date>`.
   If the 30-day projection exceeds ~2,000, find out why before it becomes an
   outage. The per-job table at the bottom of its output is where to aim.
2. **Watch for the silent-stop failure mode.** A job that fails in under ~10
   seconds with no logs is a billing or permissions failure, not a code
   failure. `gh run view <id>` shows the annotation.
3. **Green CI is not the gate — a green pre-push is.** If you find yourself
   pushing to see what CI says, the local gate is either too slow or too
   broad; fix that rather than routing around it.
4. **Keep the two filter lists in sync.** `scripts/pre-push.sh`'s `match`
   patterns mirror the `changes` job in `ci.yml`. Widen one, widen the other,
   or a green local run stops predicting a green CI run.
5. **Re-enabling a shelved gate means editing two places.** The commented
   blocks at the bottom of `ci.yml` (mesh-dst, clippy, deny, xtask gates) must
   also be added to `ci-ok`'s `needs` and its `check` list, or they will run
   without gating anything. For scale: mesh-dst and clippy measured ~29 billed
   minutes *each* per run — together, more than every non-CI workflow in the
   repo combined.
6. **Branch protection is not on.** When it goes on, require **`CI OK`** and
   nothing else. Requiring `test` directly would wedge every docs-only PR at
   "Expected — waiting for status," because a path-filtered job reports as
   skipped rather than successful.

---

## 6. What the new gate found immediately

Worth recording, because it is the clearest evidence that the gap was real
rather than theoretical. The first end-to-end run of the pre-push hook against
`main` (76785f28) failed on two gates:

- **`cargo fmt --all --check`: 144 files.** Not one PR's drift — format debt
  re-accumulated across the workspace while the gate was down. (Cleared with
  `cargo fmt --all`; 150 files. Note it needed **two** passes — one file's
  reformat opened up a further reformat, so a single `cargo fmt --all` does not
  guarantee a clean `--check`.)
- **`docs-gate`: `SYSTEM_OVERVIEW.md` cites `target/`,** which
  `build_path_index` prunes and which is gitignored regardless. Fixed by an
  allowlist entry with a reason, matching the existing `vendor/` precedent.

Both landed on `main` during the window when CI was aborting in four seconds.
That is what "the gates are shot" actually cost.

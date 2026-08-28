# Deletion — removing the mass without removing a behaviour

**Status:** pre-registration. Bars, floors and falsifiers are declared in
`quality/campaigns/deletion.toml` BEFORE the first file is deleted, and the
file set is computed by `scripts/deletion-manifest.py`, not chosen by whoever
is executing. Numbers here were measured 2026-08-27 at `2bbcb480`.

**This document exists because the order was wrong and the work is still
worth doing.** The order was: *delete 100,000 lines of code; there is that
much fat and redundancy*. There is not. Three independent read-only surveys
closed that question the same afternoon. What there is instead is **780,853
lines of committed machine output**, and about **40,000** of genuinely
deletable code. Both numbers are below.

---

## 1. The premise, corrected and measured

| Lane | Ceiling | Why the number is trustworthy |
|---|---:|---|
| Duplication | ~14,400 net | Five passes. **0** byte-duplicate files, **0** duplicate skeletons. The near-clone total reproduces this repo's own `nc-redundant` figure (528 clusters vs 540) by a completely different algorithm — token shingles against embedding cosine |
| Dead code | ~9,800 (bank ~5,500) | **Nine** hunted classes returned literal zero |
| Rust test mass | ~8,000 | 296,545 test lines, only 2,691 exact-duplicate function bodies. The `#[ignore]` lane yields **0** — all 61 are documented live-model probes |
| Vestigial crates | 1,320 | Every crate on the suspect list has symbol-level consumers in a shipped host |
| Examples + orphan scripts | 15,369 | No CI lane builds examples; `--all-targets` only *checks* them |

**The orphan-file class is closed.** The `worker_pod_provider.rs` defect — a
`.rs` on disk with no `mod` declaration, compiling into nothing — cost 394
dead lines once. Two independent checks agree it has not recurred: a BFS over
the module graph seeded from every `lib.rs` / `main.rs` / `bin` / `tests` /
`benches` / `examples` / `build.rs` reaches **2,004 of 2,004** tracked
non-vendor `.rs` files. Do not spend a day re-hunting it.

**Convergence cannot deliver this and never will.** Measured on the merges:

| Merge | Net first-party `.rs` |
|---|---:|
| Noun convergence (#47) | **+33,132** |
| Topology final three (#49) | +622 |
| `fix(mesh)`: dead auto-collaborate loop | **−3,789** |

Convergence introduces an abstraction and then removes call-site copies of
10–50 lines each; the abstraction outweighs them. The only merge that deleted
removed a dead subsystem. Over 90 days the workspace ran +621,984 / −178,784
— it deletes 29 lines per 100 added. **Deletion work and convergence work are
different instruments. This campaign is the first one.**

---

## 2. What this campaign is NOT

**It is not the JSON re-encode.** 2,166,456 of the repo's 2,189,222 tracked
JSON lines are pretty-printed and collapse to zero newlines when compacted.
`sovereign/router/router-embed-cache.json` alone is 435,030 lines of
one-float-per-line, and the round-trip is provably exact
(`json.loads(compact) == original`, 423 entries × 1024 dims). It is
`include_str!`'d at `sovereign-core/src/router_embed_cache.rs:87` into a plain
serde struct, so compact and pretty parse identically and no reader changes.

That is worth doing for git health — the file has **8,991,806 lines of churn
in 90 days**, the worst in the repo. It is **not** deletion, it deletes
nothing, and it must never be reported as progress against this campaign. It
gets its own commit and its own sentence. Applying it broadly also destroys
diffability on files humans read; machine-written artifacts only.

**It is not a refactor.** No lane here converges, extracts, renames or
introduces a type. A deletion commit that also "cleans up nearby stuff" is
ARCH §10.2's smell and will be rejected in review.

---

## 3. The manifest is the authority

`scripts/deletion-manifest.py` decides which files this campaign deletes.
One implementation (ARCH §10.6). Every bar in the campaign shells to it.

```
scripts/deletion-manifest.py --all                       # every lane, JSON
scripts/deletion-manifest.py --lane p0-root-junk --files # the exact paths
scripts/deletion-manifest.py --verify                    # vs the frozen baseline
```

The landed diff of a phase **must equal `--lane <l> --files` exactly**. Not a
superset, not a subset. Rules are code and spares are code, and every spare
cites the reader that keeps its file alive:

- `peek_budget.json`, `pre_reconciliation.json` — opened by literal path at
  `bench_cmd/enron.rs:349` and `:736`
- `runs/serve50-availability/peer-{busy,idle}_*.log` — cited by
  `quality/initiative-bars.toml:1817` as the evidence for a banked verdict

Widening a lane or adding a spare is a **diff to that script**, reviewed like
any other change. That is the mechanism, not a formality: it converts "the
agent decided to skip a file" into a reviewable line with a citation.

Frozen counts live in `quality/baselines/deletion_manifest.tsv`. `--verify`
fails if any lane **grows**, which is the ratchet.

---

## 4. The phases

Sequenced by lines per unit of risk. Totals from the frozen manifest.

| Phase | Lane | Lines | Files | Risk |
|---|---|---:|---:|---|
| **P0** | `p0-root-junk` | **113,934** | 147 | zero — nothing reads any of them |
| **P1a** | `p1-bench-baselines` | **384,759** | 161 | low — unreachable by construction |
| **P1b** | `p1-dr-flights` | **272,421** | 2,118 | low — policy already written |
| **P2** | `p2-code-certain` | **9,739** | 34 | low — file-granular, compiler-checked |
| | **total** | **780,853** | 2,460 | |

**P0 — committed process output.** Root `score-report-*.json` (arrived in
commit `8bdefaa6`, message: *"stash dr work"*), stray `.log` captures
including two accidental Playwright console dumps inside `sovereign-tools`,
the stray root `baselines/` directory untouched since 2026-06-09, and six
files committed **both** raw and gzipped.

**P1a — baseline snapshots the reader cannot address.**
`bench_cmd/baselines.rs:39` builds every path as
`bench_root/<group>/baselines/<id>/` and `:44` opens `latest.json` inside it.
So a file sitting **flat** in a `baselines/` dir has no `<id>/` and is
unreachable *by construction* — that is 62 files and 269,928 lines on its own.
Inside an `<id>/` dir, only `latest.json` and its symlink target are ever
opened. **Falsifier:** `svrn bench gate` must return an identical verdict for
every bank before and after.

**P1b — runtime bookkeeping in committed flight trees.** Gap lists,
checkpoints, budget and skip ledgers, resume state, console capture.
`research/deep-research/arms/.gitignore` already states the policy — *"Flight
trees are EVIDENCE, not source… the run trees stay local"* — and three trees
obey it while sixteen were committed anyway between 2026-08-17 and 08-21.
This lane enforces a decision already taken.

`charter.json` (each run's **pre-registration**), `report.md` (the finding),
`plan.json` and `survey-*.json` are **deliberately excluded**. The aggressive
variant adds ~22,000 lines and deletes pre-registrations. This workspace does
not delete pre-registrations. **Falsifier:** every bar in
`quality/campaigns/drb1-race.toml` prints a byte-identical value.

**P2 — dead code at file granularity.** `sovereign-store/src/postgres.rs`
(1,522 lines) plus 33 individually-cleared examples (8,217).

`postgres.rs` has three independent proofs: no manifest, CI lane or script
enables the `postgres` feature; `traits.rs:1760` requires `DocumentAssetStore`
and the file's 12 impls omit it while `:1522` asserts `impl StateStore for
PostgresStateStore {}`, so it **cannot typecheck**; and `quality/CLEANUP.md`'s
2026-07-12 record already says *"`--features postgres` was ALREADY broken…
will keep flagging it until fixed or the feature is retired."* Whether to keep
a Postgres backend is a product decision — take it to the operator once, then
delete. **Falsifier:** `cargo clippy --workspace --all-targets` must not emit
one new `dead_code` warning above `quality/baselines/clippy_counts.tsv`.

**P3 — the remaining ~30,000 of code, NOT in the manifest.** Duplication
clusters needing no new crate or dependency edge (~11,300: the 11 identical
axum ack-handlers at `corpus_watch_http.rs:963`, the four exemplar classifiers
sharing a 12-method skeleton, `recipe_builtin`'s five-place table), Rust test
dedup (~8,000), env-flag dead paths already costed per-variable in
`docs/ENV_VAR_AUDIT.md`, the five undocumented hidden CLI verbs (1,512),
`oicp-conformance` (1,320), and the 130 zero-caller functions whose name
occurs exactly once repo-wide (1,285).

These are **not** machine-decidable and deliberately have no manifest lane.
Each needs one `cargo check -p <crate>` and human eyes. Do not automate them.

---

## 5. Entry gate: the positive control

**Run this before deleting anything, and again at the head of every phase.**

```
scripts/deletion-manifest.py --lane p0-root-junk --files | head -1   # pick a victim
# move it aside, re-run --all, confirm the total drops by exactly its line count
```

The `positive-control` bar automates it. It must print `"value": 1.0` with
`expected == moved`.

This is not ceremony. `quality/HOT_PATH_REUSE.md:102` records that
`nc-redundant` **was 79% blind to deletion for its entire life** — 8,117 of
10,301 lines could not move by the deletion the bar existed to reward — and
that nine waves of work were spent before anyone checked. The check costs
thirty seconds: *delete a known item, confirm the number moves by exactly that
delta.* An instrument that cannot see deletion makes every number below it
unearned.

---

## 6. Traps — measured, each one costs a day

1. **Never take SCIP's zero-caller list at face value.** Trait-impl methods
   reached through `Arc<dyn Tool>` record the reference against the *trait's*
   symbol, never the impl's. **1,147 functions / 35,339 apparent LOC** look
   dead and are all live.
2. **Never run a reachability closure on that graph.** Because trait impls
   look dead, everything they call looks dead. A closure run "proved"
   `parse_message`, `collect_body`, `persist_state` and four others dead; every
   one is called in its own file. Direct-inbound only.
3. **Bodiless trait declarations inflate LOC.** SCIP gives
   `async fn foo(&self) -> Result<T>;` a `line_end` at the close of the
   enclosing `trait`. `sovereign-atos/src/lib.rs:291-360` reported twelve dead
   functions totalling ~700 LOC; they are twelve one-line signatures.
4. **`prost` and `serde` are reflective.** `corpus-engine-scip/src/scip_proto.rs`
   is decoded by the protobuf runtime — zero refs by construction.
   `#[derive(Deserialize)]` field structs are the same.
5. **A name appearing elsewhere proves nothing in either direction.**
   `export_changed` appears in `facts_store.rs:575` and `facts_tool.rs:296` —
   as *string data in test fixtures*, not calls. Both the naive "it's
   referenced, keep it" and "it's referenced, so my dead-list is wrong" reads
   are wrong.

One schema trap worth carrying: `symbols.kind` and `refs.ref_kind` in the
SCIP DB are junk — 288,909 of 325,873 rows are `kind='unknown'`, there are
**zero** `kind='function'` rows, and `ref_kind` is hardcoded `'direct'`. The
only reliable discriminator is the `qualified_name` descriptor grammar.

---

## 7. Diversion register

**Every entry below was predicted before execution started.** If you are
executing this campaign and about to do one of these, the answer is already
here. Reopening one requires new measurement, not a better argument.

| The diversion | Why it will tempt you | The measured answer |
|---|---|---|
| *"Minify the JSON — that's 2.1M lines"* | It is real, it is huge, and it works | It deletes **nothing**. §2. Separate commit, separate sentence, never campaign progress |
| *"While I'm in here, converge these duplicates"* | The duplication is genuinely there | Convergence is **net-additive**: #47 was +33,132. ARCH §10.2. Not this campaign |
| *"These baselines might be needed for historical comparison"* | Deleting history feels irreversible | The reader **cannot address them** — `baselines.rs:39` always appends `<id>/`. Git history still holds them |
| *"The tests look redundant — that's 296k lines"* | It is the biggest Rust number on the board | Measured: **~8,000** net. Only 2,691 exact-duplicate bodies. The `#[ignore]` lane yields **0** |
| *"`deep_research` is 22,427 lines and looks abandoned"* | It is large and self-contained | It ships as a **default-on user verb** (`sovereign-cli` default features). LIVE |
| *"SCIP says 1,147 functions have no callers"* | The tool said so | Trait dispatch. 35,339 apparent LOC, **all live**. Trap #1 |
| *"Let me run a closure to find more dead code"* | It would find much more | It cascades and is wrong. Trap #2 |
| *"I found more files matching the pattern — widening the lane"* | It looks like the same class | Widening is a **diff to `deletion-manifest.py`** with evidence, reviewed. Never at the keyboard |
| *"I'll skip this one file to be safe"* | Caution feels free | A spare without a cited reader is refused. If it has one, it is a **manifest bug** — add it, re-freeze, say so |
| *"Delete the whole `research/` tree"* | 864k lines, one directory | `charter.json` is each run's pre-registration; `report.md` is the finding. §P1b |
| *"Tidy the wrong docs while I'm here"* | 20 files still describe the deleted `commonwealth-daemon` | A wrong doc is an **edit**, not a delete. Different lane, different commit |
| *"The gate is flaky / unrelated"* | It will go red at some point | Gate on the **exit code**. Zero-test runs exit 4, unattributable runs exit 5 — both are failures (ARCH §18.1). Bisect the phase's own diff |
| *"The doc's numbers don't match what I see"* | They will drift as the campaign lands | `--verify` is the authority. A lane that **shrank** is progress; one that **grew** is a defect to find |
| *"100k is the target, I'm short — find more"* | The original number | The original number was **wrong**, and §1 says why. Report what was deleted; do not manufacture scope to hit a figure nobody measured |

---

## 8. The ratchet — the actual deliverable

| Month | Net first-party Rust |
|---|---:|
| 2026-04 | +298,125 |
| 2026-05 | +265,160 |
| 2026-06 | +125,583 |
| 2026-07 | +165,587 |
| 2026-08 | +155,452 |

**100,000 lines is 19.6 days of accretion.** Every lane in this campaign
refills unless something stops it. `--verify` is that thing: it fails when a
lane grows, so committed output cannot silently return while the campaign
runs.

After P2 lands, promote it. `corpus-engine/xtask/src/arch_gate.rs` already has
the shape — a frozen `name → count` baseline, `--tighten` that banks
improvements and never raises, registered in `quality_cmd.rs`'s gate table
with `Enforcement::Hard`. An `artifact-gate` over tracked-output line classes
is the same mechanism pointed at a different census, and reusing it is the
point (ARCH §19: the inventory outranks the plan).

---

## 9. How this document stays honest

1. **Every falsifier above is an executable bar**, not a sentence. A falsifier
   that stops running marks its row a target.
2. **The four verdicts apply per phase** (ARCH §18.2): passed, failed,
   could-not-judge, **never-ran**. A phase nobody executed is `never-ran` and
   says so — it is not silently absent.
3. **The manifest and this prose can disagree, and the manifest wins.** If a
   number here contradicts `--verify`, this file is stale. Fix it in the same
   commit as the phase that moved it (ARCH §1.1).
4. **Report what the diff did, not what the plan hoped.** A phase that deleted
   80,000 of 113,934 reports 80,000 and names what it skipped and why.

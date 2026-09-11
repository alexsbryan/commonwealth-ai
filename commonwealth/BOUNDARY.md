# The commonwealth package boundary

`commonwealth/crates/commonwealth-{core,transport,state,discovery,rail-core,rail,work}`
holds the **mesh substrate** — the crate set a third party could lift out of this
monorepo and build a peer against, with no sovereign runtime, no corpus engine
and no model. Seven crates, 22,159 lines across 54 files (16,651 across 41 in the
four founding members; the rest is the rail and the work plane, admitted
2026-09-04 and 2026-09-09). This document is the contract;
`cargo run -p xtask -- boundary-gate` enforces it (blocking, one of the eight
pre-push ratchets), and the declaration lives in `quality/ARCH_LAYERS.toml`.

## Why this is declared before the work, not after

The initiative that motivates it moves ~5,000 lines out of these crates and
extracts a rail from `commonwealth-knowledge`. Every gate in the repo would go
green on the *first* half of that work — the half that moves files — and stay
green on a `sovereign-*` dep acquired the day after. `layer-gate` cannot see it:
its "no sovereign" property is seven hand-enumerated per-crate `[[forbid]]`
blocks at `quality/ARCH_LAYERS.toml:305-341`, and a crate added to the
`mesh-foundation` layer list gets no forbid rule and prints green. A package
declaration is the instrument that makes the claim falsifiable, so it lands
first (ARCH §18.1 — a gate you have not watched fail is not a gate).

It is green on the day it is declared, which is unusual for a boundary and
worth saying plainly: the four crates' entire internal dependency surface is
`commonwealth-core`, `kernel-types` and `oicp-types`, and the latter two are
already shared leaves. There are no grandfathered `[[exception]]` rows. That is
the property the gate now holds, not one it is aspiring to.

**It survived its first widening.** The rail joined on 2026-09-04 (cw-lift 1b),
taking the package from four crates to six, and the zero-exception property
held — `boundary-gate` reads `commonwealth 6/6 crates present` with no new row.
That cost one leaf admission, `oplog`, and the admission was measured rather
than argued: the global leaf union already carried 158 crates, oplog's closure
is 28, and the crates oplog adds that no existing leaf already carries number
exactly one — oplog itself. A widening that admits a crate and nothing else is
the cheapest shape this list can take.

**And its second, on 2026-09-09** (cw-lift 5c): `commonwealth-work` took the
package from six crates to seven, `boundary-gate` reads
`commonwealth 7/7 crates present` with no new `[[exception]]` row, and the
zero-exception property held again. This one cost NO leaf admission — see the
measurement below — and it is the first crate admitted here *before* it has a
consumer, deliberately: cw-lift 5f lifts this closure out of the monorepo and
builds a third-party peer against it, and every gate in the repo would be green
on a `sovereign-*` edge acquired the day before that lift. The gate was watched
red to prove it binds: a `sovereign-core` dependency added to the manifest
makes `boundary-gate` report
`commonwealth-work -> sovereign-core: a normal dependency leaves the package
closure` and `layer-gate` report the `[[forbid]]` row by its reason text
(ARCH §18.1).

**THAT RED WATCH PROVED LESS THAN IT READ AS, and the correction is worth more
than the claim.** `sovereign-core` is the one sovereign crate that is not a
declared `[[package_leaf]]`, so it was caught by MEMBERSHIP — the rule the gate
already had. Re-run on 2026-09-09 with `sovereign-contracts`, which *is* a
leaf: `layer-gate` exit 1 (the `[[forbid]] commonwealth-work -> sovereign-*`
row, plus `fan-in of sovereign-contracts grew 26 -> 27`), and `boundary-gate`
exit **0**, printing `commonwealth 7/7 crates present` and `✓ every declared
package reaches only itself + the shared leaves`. Leaves are GLOBAL — admitting
one widens every package at once — so `sovereign-contracts` being liftable with
`studio` made it liftable with `commonwealth` too, whose entire declared
property is that it names no `sovereign-*`. The per-package refinement lives in
the `[[forbid]]` table, and the package pass never read it.

Fixed 2026-09-09: `arch_layers::forbidden_by` is now the one decider both
passes call, and a `[[forbid]]` row outranks package membership and the
shared-leaf allowance alike. Watched red with the `sovereign-contracts` edge
before it was trusted, and pinned by
`a_forbid_row_outranks_package_membership_and_the_shared_leaf_allowance`
(`quality/arch-layers/src/packages.rs`), whose case 0 is a negative control
asserting the leaf allowance really does admit the edge — without it the suite
would pass against the old code. **The general lesson: watch a gate red with an
input drawn from the class it must catch, not the first member of that class
that comes to hand.** Every `sovereign-*` crate looked equivalent for this
purpose and two of them were not.

## The two tiers

**Package crates** (`commonwealth/crates/`):

| Crate | Lines | Closure | Role |
|---|---:|---:|---|
| `commonwealth-core` | 8,860 | 55 | Identity, roster, capabilities, the mesh clock, the shared vocabulary. |
| `commonwealth-transport` | 2,746 | 57 / 265 | The peer wire — the direct TCP path, and iroh behind an optional feature. TWO closures because it ships in two configurations: 57 default, 265 with `iroh`. Three in-repo consumers force the feature on, and under resolver-2 unification any `--workspace` gate run builds the 265 — so the DEFAULT is the configuration this repo's own gates never compile. The 2026-09-04 lift is the first thing that did; it builds, and carries 23 tests where the iroh path carries 39. |
| `commonwealth-state` | 2,360 | 76 | `MeshStore` and the replicated state it holds. |
| `commonwealth-discovery` | 3,228 | 82 | Founder/joiner, announce, the peer table. |
| `commonwealth-rail-core` | 2,560 | 42 | The fold: vocabulary, Ed25519 authorship, admission into one total order, the per-actor sync digest and its sealed floor. Zero I/O. |
| `commonwealth-rail` | 740 | 43 | The journal: the append-only JSONL log under `<root>/rings/<ns>/`. |
| `commonwealth-work` | 1,210 | **58 / 70** | The work plane: the `WorkAct` codec, the unit seal, the fold, the one lease predicate, the executor seam and (since 2026-09-10) `attribution` — the one reader of "what host am I", which three implementations had. TWO closures, like `commonwealth-transport`: 58 by default and 70 with the `process` feature, whose entire cost is tokio's twelve crates (`tokio`, `tokio-macros`, `mio`, `bytes`, `socket2`, `parking_lot` and friends, `signal-hook-registry`, `errno`, `scopeguard`, `smallvec`, `lock_api`). The core — codec, seal, fold, predicate — is zero I/O and zero clock, and that manifest split is how it is enforced rather than remembered. |

`commonwealth-work` measured 2026-09-09 the same way (`cargo tree -e normal -p
<crate> --prefix none`, unique package names, the crate itself excluded); that
recipe reproduces the recorded 55 / 42 / 43 for `-core` / `-rail-core` /
`-rail` exactly, which is why the new number is trustworthy and why `-state`
and `-discovery` reading 78 and 79 today against the 76 and 82 recorded here is
drift in THOSE rows rather than a different instrument. **The widening admits
nothing at all**: `commonwealth-work`'s 58-crate default closure is
`commonwealth-core` ∪ `commonwealth-rail-core` plus those two crates
themselves, and nothing else — 58 = 56 + 2, verified by set difference in both
directions on 2026-09-09 rather than by comparing counts. The phrasing matters
because the recipe above EXCLUDES the crate itself, so "set-equal" read
literally is off by exactly the two package crates every time and a later
reader re-measuring would think the number had drifted. The one crate
`commonwealth-work` adds to the package's own closure is itself.

The **dev** closure is 59 / 71 (`-e normal,dev`), and the one crate above the
normal figure is `commonwealth-rail` — a dev-dependency since cw-lift 5f,
because the package-only peer at
`commonwealth-work/examples/work_peer.rs` has to OPEN a journal and the fold
never does. It is a package crate, so the rule that dev-dependencies count is
satisfied without an `[[exception]]` row and without a leaf admission, the same
way 5c's own widening was. That is cheaper than the `oplog`
admission the rail cost. Its line figure is `wc -l` over `src/**/*.rs` — the
same shape as the rows above it, which have drifted since they were written
(`-rail-core` reads 2,771 by that measure today against the 2,560 recorded).
The ratcheted split, from `cargo xtask size-gate`, is 357 code lines and 316
test lines with comments and blanks excluded.

Closures measured 2026-09-03 with `cargo tree -e normal`, third-party included;
the two rail crates re-measured at the sealed floor (2026-09-04) and both are
unchanged — a `RailAct` variant and one derivation add no edge. Their line
figures move with that commit; `commonwealth-rail-core` read 2,270 before it
and not the 2,258 recorded here, a twelve-line drift of the same kind 1e
corrected for `commonwealth-rail` and corrected in passing again.
`commonwealth-discovery` came DOWN 101 -> 82 on 2026-09-04, and not from a
module deletion. Phase 0 removed the TLS and gossip-peer-selection modules and
left their dependencies in the manifest: `rcgen` and `rand` had zero references
in `src/` or `tests/`, under comments still naming the subsystems that used
them. Dropping the two lines cut 16 crates from the closure. Found while
writing this package's crate docs — nobody looked for it, and no gate can:
`cargo` does not warn on an unused dependency, and the layer map governs which
edges are LEGAL, never which are LIVE. Worth a periodic sweep of every package
crate, since a deletion campaign that removes code and leaves manifests keeps
paying for what it deleted.

**Shared leaves** — the global `[[package_leaf]]` set. The commonwealth package
takes exactly three of them, and none is a concession:

| Crate | Allowed internal deps | Why it may cross |
|---|---|---|
| `kernel-types` | *(none)* | Identity + provenance. `ContentHash` is wire-critical here — node and op ids are gossiped. |
| `oicp-types` | `kernel-types` | The wire vocabulary. A protocol crate a peer already has to speak. Since cw-lift 5b it also carries the JOB vocabulary — `JobKind`, `JobUnit`, `JobRequirements`, `Isolation`, `WorkOffer`, `JobExecutorDescriptor` — which is what `commonwealth-work` is written against. Its one in-repo edge is `kernel-types`, itself an empty leaf, so the pair costs the package a two-crate closure. |
| `oplog` | `kernel-types` | The append-only journal the rail folds over. Admitted 2026-09-04 so the rail could join the package at all — see the measurement above. It owns ordering and dedup, never identity, which is why `kernel-types` is its one internal dep. |

## The rules

1. **A package crate may depend only on other package crates + the shared
   leaves.** No `sovereign-*`, no `corpus-engine*`, no `commonwealth-knowledge`,
   `-inference` or `-api` — those three sit at 577-690-crate closures because
   they name `corpus-engine`, and they are *applications on* this substrate, not
   part of it.
2. **`sovereign-mesh` is an application too, and never a second substrate.**
   This is the load-bearing rule of the whole boundary. `sovereign-mesh` holds
   substrate-shaped code — a `MeshStore` replication path, a founder/joiner
   handshake — and the temptation is to extract it into a peer of these four
   crates. Do not. Its substrate-shaped names have approximately no external
   consumers (~15 of ~234 crossing refs), so extracting them would fork the
   deciders this package exists to own: two replication paths, two orderings,
   two admission rules. That is ARCH §10.6 at crate scale. The substrate has one
   home and this is it; substrate-shaped code found elsewhere is **rehomed here
   or deleted**, never mirrored.
3. **The shared leaves keep their global budget.** Widening one widens every
   package's contract surface at once.
4. **No `build.rs`, and no `include_str!`/`include_bytes!` escaping the crate
   root.** `commonwealth-core` now has NO embeds at all: `default_pipelines.toml`
   left with `pipeline_aliases` for `serving-policy` (cw-lift 4a, e26414742) and
   `default_aliases.toml` left with `model_aliases` for `oicp-types` (cw-lift 4b).
   Both are still crate-local in their new homes, which is what the rule asks.

The rules count **dev- and build-dependencies too**: a third party who lifts the
package carries its tests.

## What a green gate does not prove

The same caveat `studio/BOUNDARY.md` earned by actually performing a lift: a
clean dependency closure is not a clean lift. Studio's gate was green while
`sovereign-contracts` embedded a file from outside its crate root, and the
sandbox had to preserve the monorepo's directory shape to compile. The
commonwealth package has no such embed today; the way to know it still does not
is to lift it, not to read the gate.

**So it is lifted on demand now, and not by hand.**
`scripts/cw-work-lift.sh --sandbox` is the standing instrument for the
`cw-work-package-lift` bar, and it does what the 1f' lift did once in a
terminal: it walks the workspace-local closure from `commonwealth-work`
through `[dependencies]`, `[dev-dependencies]` and `[build-dependencies]`,
copies those crates to a scratch directory OUTSIDE this repository, synthesises
a root workspace there, and builds, tests and RUNS them. Three things about it
are deliberate:

- **The copy is FLAT** — `crates/<name>`, not the monorepo's directory shape.
  Preserving the shape is what studio's sandbox had to do, and it proves the
  crates compile where they already are, which is not the question. Flattening
  is also what makes a hand-spelled `path = "../../../oicp-types"` fail
  instead of passing, which is the 1f' finding the gate is blind to; the script
  refuses that spelling by name before it copies anything.
- **The sandbox is outside the repo, and that is load-bearing.** `cargo`
  inherits `.cargo/config.toml` from ANY ancestor directory, so a sandbox under
  the repo would silently take this workspace's linker and rustflags. Neither
  `.cargo/config.toml`, `clippy.toml` nor `rust-toolchain.toml` travels: a
  third party has their own.
- **A missing instrument does not score 0.** A resolution that cannot be done
  (no `cargo`, no network, an unresolvable version) exits 3 and makes no claim;
  a closure that genuinely will not build exits 0 with the value 0. The two
  readings are not the same fact (ARCH §18.2, §18.3), and each arm has been
  watched fire.

**Measured 2026-09-09, on Linux/x86_64.** Seven in-repo crates leave together —
`commonwealth-core`, `-rail`, `-rail-core`, `-work`, `kernel-types`,
`oicp-types`, `oplog` — and the whole workspace builds cold in **8.6-12.5s**
(n=4 cold runs, `RUSTC_WRAPPER` cleared) with **zero source edits**. A range
and not a number, because one run is not a measurement (ARCH §18.5) and the
spread here is the host's page cache, not the closure. `cargo test --workspace` passes **558** tests in
isolation. The peer then completes **three heterogeneous `process:v1` units** —
a `sh -c` one-liner, a Python Monte-Carlo simulation that judges its own error
bound, and a test shard running the lifted package's own `cargo test` — leaving
nine acts on a `work` rail it created itself: a `Submit`, an `Offer`, three
`Lease`/`Complete` pairs and one `Renew` that the 27-second shard earned.

**The `kernel-types` hazard is closed, and the lift is how we know.** The 1f'
lift failed `490 passed, 2 failed` on two leaf-side tests that read the repo
root — `requirements_registry.rs:94` through `CARGO_MANIFEST_DIR.parent()` and
`conformance_tags.rs:80` shelling `git ls-files`. Both generators moved to
`corpus-engine/xtask/tests/` on 2026-09-04, and the 5f sandbox scan finds no
`CARGO_MANIFEST_DIR` read, no `git ls-files` and no `include_str!` escaping a
crate root anywhere in the lifted closure. `oicp-types`' embed of
`default_aliases.toml` is crate-local, which is what rule 4 asks for. The
campaign's own bar text still names that hazard as open; it is not, and this is
the run that says so.

Two further blind spots, both known:

- **The closure count has no ratchet.** `boundary-gate` checks membership, not
  size. A leaf that grows a dependency widens every package silently. The
  numbers in the table above are recorded here so a reader can diff them by
  hand, which is weaker than a gate and is the honest state.
- **`iroh` is optional here and forced elsewhere.** `commonwealth-transport`
  gates it behind a feature, but `sovereign-server` turns it on for the default
  build. The package boundary cannot see a feature another crate enables; the
  local-only-daemon claim is proven by building the daemon, not by this gate.

## When the gate fails

Declare the edge or delete it. `[[exception]]` rows carry `package =
"commonwealth"` and a reason; they are a counted ledger, and a stale one — the
edge is gone — fails the gate until it is deleted. Removals are the
celebration.

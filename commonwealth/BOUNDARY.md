# The commonwealth package boundary

`commonwealth/crates/` holds the **mesh substrate** — the crate set a third
party could lift out of this monorepo and build a peer against, with no
sovereign runtime, no corpus engine and no model.

**Which crates those are is not written here.** The set lives in exactly one
place, `[[package]] name = "commonwealth"` in `quality/ARCH_LAYERS.toml`, and
this file describes what the package IS rather than keeping a second copy of
its membership. That is deliberate and it is a correction: between 2026-09-04
and 2026-09-11 the package was widened four times and this paragraph went on
saying "seven crates, 22,159 lines across 54 files" while the declaration held
nine and 37,363 — a second copy of a list, drifting, with the gate green the
whole time because the gate reads the declaration. The fix is one list, not a
checker over two (§10.6). Read the declaration; it is thirty lines and each
admission carries its reason.

The commentary below — roles, closures, what each widening cost — is
commentary, and it is allowed to lag a crate without lying, because it no
longer claims to be the set.

This document is the contract for the PROPERTY;
`cargo run -p xtask -- boundary-gate` enforces it (blocking, one of the eight
pre-push ratchets).

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

**And its third and fourth, on 2026-09-11** (cw-lift D1): `commonwealth-media`
took it to eight and `commonwealth-rails` to nine, and the zero-exception
property held through both — `boundary-gate` reads `commonwealth 9/9 crates
present` with no new `[[exception]]` row and no new leaf, because media's whole
internal surface is `commonwealth-core` + `commonwealth-transport` and the
daemon's is those two plus `-media` and `-discovery`, every one already a
member.

`commonwealth-rails` is the first BINARY admitted here, and that changes what a
green gate is worth. `cargo tree` cannot see a `build.rs`, a hand-spelled
`path = "../.."`, or an `include_str!` escaping the crate root, and a daemon is
where those appear. `boundary-gate` reads three of those four itself (build.rs,
include_str escapes, runtime root escapes, over in-repo package and leaf
directories); what only a LIFT can read is whether the copied closure actually
resolves, builds and runs. It did, 2026-09-11: `scripts/cw-rails-lift.sh
--sandbox` copied 7 in-repo crates out, resolved 518 packages, built in 22.1 s
on a stock toolchain with none of this workspace's `.cargo/config.toml`,
`clippy.toml` or `rust-toolchain.toml`, passed 37 tests in isolation, then
JOINED a real mesh by invite and pulled HTTP 302 from another member's Jellyfin
origin in 18 ms.

**And the fourth widening made a correction the first three did not.**
`[[forbid]] from = "commonwealth-rail*"` — written to cover the two rail crates
— also matches `commonwealth-rails`, which is named after the RAILS and not the
rail, and `forbidden_by` returns the FIRST match. So the daemon silently
inherited four rules written for something else, including a ban on
`commonwealth-core`, its own substrate, and its own rows were never reached.
Each rule now names its two crates explicitly. A glob over crate names is a
decider keyed on a prefix, and prefixes are not identity (§7.5).

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
| `commonwealth-media` | 1,148 | 268 | Federated media: who offers a library (`OriginKind`), who may reach one (`media_allow`), the viewer bridge whose port is derived from the peer key, and the catalogue fan-out. Its whole in-repo surface is `commonwealth-core` + `commonwealth-transport`. The closure is large for one reason — it forces `commonwealth-transport`'s `iroh` feature, which is 256 of the 268 — so the number prices the transport, not this crate. |
| `commonwealth-rails` | 3,120 | 297 | The rails daemon a shim author installs: `cw-rails join <invite>` and `cw-rails run`. One iroh endpoint carrying the join handshake, the gossip round, the acceptor and every media bridge, plus `POST /internal/gossip` so full daemons stop marking this member Offline, and three loopback routes. The FIRST BINARY in this package, which is why the physical lift matters more here than anywhere else. Same `iroh` story as `-media` above; against `commonwealth-api`'s 694, which is the comparison that means something. |

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

**It WAS lifted on demand and is by hand again, 2026-09-11 — read the next
section before you trust this paragraph.** `scripts/cw-work-lift.sh --sandbox`
was the standing instrument for the `cw-work-package-lift` bar until the
cw-lift campaign closed and its file moved to `quality/campaigns/closed/`,
which `co-lineage.py` skips by construction. The script is unchanged and still
correct; what it no longer has is a caller. It does what the 1f' lift did once
in a terminal: it walks the workspace-local closure from `commonwealth-work`
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

## What is enforced, and what is not

Written 2026-09-11, when closing the cw-lift campaign removed the physical
lift's only trigger and nothing went red. The boundary has two tiers and they
prove different things; conflating them is how a package rots while its gates
stay green.

**Tier 1 — the declaration, enforced on every push, blocking.** Three gates,
none of which needs a human to remember anything:

| Gate | What it reads | What it would catch |
|---|---|---|
| `boundary-gate` | the manifest graph of every `[[package]]`: normal + dev + build edges, `build.rs` presence, `include_str!` escapes, runtime root escapes, over in-repo package and leaf dirs | a package crate acquiring an edge to anything that is not a package crate or a shared leaf |
| `layer-gate` | the `[[forbid]]` table, which since 2026-09-09 OUTRANKS package membership and the shared-leaf allowance alike | a `sovereign-*` edge that membership alone would permit because the target is a declared leaf — the `sovereign-contracts` case that the first red watch missed |

This tier is what stops an edge being acquired BETWEEN lifts, and it is real:
zero `[[exception]]` rows, held across four widenings.

**A third gate was written for this on 2026-09-11 and then deleted the same
day**, and the reason is worth more than the gate was. It checked that every
crate in a `[[package]]`'s list is named in the doc that package declares, and
it worked — it found this file stale by two crates and `corpus-mcp/README.md`
stale by one. But a checker that keeps two copies of a list in agreement is
policing a duplication instead of removing it, and the repo already has enough
after-the-fact enforcement. The membership list now exists once, in the
declaration, and this file stopped restating it. There is nothing left to
check, which is the better shape (operator direction, 2026-09-11; ARCH §10.6,
§7).

**Tier 2 — the physical lift, enforced nowhere.** `scripts/cw-work-lift.sh
--sandbox` and `scripts/cw-rails-lift.sh --sandbox` copy the closure out of this
repository, synthesise a workspace, and build, test and RUN it. They read the
four things `cargo tree` cannot: whether the copied set actually resolves,
whether a hand-spelled `path = "../.."` survives flattening, whether the crates
build without this workspace's `.cargo/config.toml`, and — for the rails daemon
— whether the binary joins a real mesh and serves. Neither has a caller:

- `cw-work-lift.sh` was reached by the `cw-work-package-lift` bar until
  2026-09-11. The campaign closed, the file moved to `closed/`, and
  `co-lineage.py`'s loader globs `*.toml` at the top level only.
- `cw-rails-lift.sh` has NEVER had one. `quality/ARCH_LAYERS.toml:548` says
  "this binary is BUILT AND RUN outside the monorepo by
  `scripts/cw-rails-lift.sh`" as the reason a `[[forbid]]` row exists, and that
  sentence describes a thing a person did once, not a thing that happens.
- Neither appears in `quality/instruments.toml`, so `instrument-gate` cannot
  see them either: it censuses commands reachable from a declared surface, and
  these are reachable from none.

**The rule this is an instance of** (`AGENTS.md`, the MCP retirement note): a
tool is called only when something in a session's flow asks its question. If
you cannot name the moment that calls it, it is inventory. Both scripts are
inventory today, and both are registered in `quality/instruments.toml` as of
2026-09-11 with `runs_in = ["by-hand"]`, which is the honest declaration and not
a fix.

**Why tier 2 cannot simply be moved to tier 1, stated rather than assumed.**
`cw-work-lift.sh` could be: it is hermetic, takes ~60 s, and needs only a
container image the operator declares. `cw-rails-lift.sh` cannot: its own
pre-registration says a verdict of **3** is could-not-judge and its expected
cause is NO INVITE — the lifted daemon cannot mint one, so its last step needs
a live mesh and a member's word. An unattended runner would report
could-not-judge forever, and a check that can only abstain is not a check.

**What would restore tier 2, ranked.** (1) A `[[bar]]` on an ACTIVE campaign
whose objective the lift serves — the mechanism that worked, lost only because
the campaign carrying it closed. (2) A `weekly:` venue row for
`cw-work-lift.sh`, the hermetic one, which needs `.github/workflows/weekly.yml`
to actually run on this fleet (`GROUND_TRUTH.md` records hosted CI dead on a
spending limit, so this is a real precondition and not a formality). (3) For
the rails lift, a step in the release procedure rather than a gate, because its
last act needs a person with an invite. None of the three is done; this section
exists so the next reader finds the gap named rather than a green gate implying
a coverage that is not there.

## When the gate fails

Declare the edge or delete it. `[[exception]]` rows carry `package =
"commonwealth"` and a reason; they are a counted ledger, and a stale one — the
edge is gone — fails the gate until it is deleted. Removals are the
celebration.

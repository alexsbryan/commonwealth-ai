# The commonwealth package boundary

`commonwealth/crates/commonwealth-{core,transport,state,discovery}` holds the
**mesh substrate** — the crate set a third party could lift out of this monorepo
and build a peer against, with no sovereign runtime, no corpus engine and no
model. 17,194 lines across 51 files. This document is the contract;
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

## The two tiers

**Package crates** (`commonwealth/crates/`):

| Crate | Lines | Closure | Role |
|---|---:|---:|---|
| `commonwealth-core` | 8,860 | 55 | Identity, roster, capabilities, the mesh clock, the shared vocabulary. |
| `commonwealth-transport` | 2,746 | 57 | The peer wire — the direct TCP path, and iroh behind an optional feature. |
| `commonwealth-state` | 2,360 | 76 | `MeshStore` and the replicated state it holds. |
| `commonwealth-discovery` | 3,228 | 101 | Founder/joiner, announce, the peer table. |

Closures measured 2026-09-03 with `cargo tree -e normal`, third-party included.
`commonwealth-discovery`'s 101 is the number to watch: four of its ten modules
are scheduled for deletion, and the closure should come *down* when they go.

**Shared leaves** — the global `[[package_leaf]]` set. The commonwealth package
takes exactly two of them, and neither is a concession:

| Crate | Allowed internal deps | Why it may cross |
|---|---|---|
| `kernel-types` | *(none)* | Identity + provenance. `ContentHash` is wire-critical here — node and op ids are gossiped. |
| `oicp-types` | *(none)* | The wire vocabulary. A protocol crate a peer already has to speak. |

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

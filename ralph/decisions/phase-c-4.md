<!-- ledger -->

**phase-c-4 · 2026-10-01 · pc-removed-env-warn · worker** — 5b6e8bbba
- Needed: a home for `promote_legacy_env` that cw-rails can call. The row's "a directly launched serve, cw-rails or pod worker skips the SVRNMESH_ bridge" is true for all three, and cw-rails' closure reads SOVEREIGN_ADVERTISE_ADDR, _MESH_STRICT_AUTH and _IROH_RELAY_ONLY. The bridge lived in sovereign-contracts, which `[[forbid]] commonwealth-rails → sovereign-*` keeps cw-rails off.
- Chose: move it whole to `kernel_types::env_bridge` (re-exported at `sovereign_contracts::rebrand`), with the removed-var table beside it; `promote_legacy_env_as(program)` so cw-rails speaks as itself.
- Because: kernel-types is the one crate already reachable from sovereign-contracts, commonwealth-rails and sovereign-serve, with `allow = []` and a std-only body: zero new edges. host-kit was the other candidate and is out twice: sovereign-contracts' package_leaf allow list is oicp-types, kernel-types, sovereign-time (quality/ARCH_LAYERS.toml:888), and host-kit "names no program's vocabulary" while the bridge's prefixes are vocabulary. Precedent: `kernel_types::member` moved here for the same two-owner reason (fp-46). BOUNDARY ✓ and clone-gate 13826 = baseline after the move.

<!-- appendix -->

## phase-c-4 · 2026-10-01 — The env-prefix bridge lives in kernel-types

<details><summary>reasoning, evidence, package</summary>

kernel-types' charter reads "identity and provenance", and an env mirror is neither; this is the stretch the choice accepts. It already carries `hardware_fingerprint` and the instrument registry, so the charter was not strict before. The brand prefix is read by commonwealth crates as well as sovereign ones, so it is owned by no single product domain, which is the kernel's actual admission rule (lib.rs: "owned by no product domain").

Falsified if: an operator rules the kernel identity-only. The alternative then is a new neutrally named leaf for process-start mechanics (an operator decision per §0, "any other new leaf"), or host-kit with sovereign-contracts' allow list widened and the prefixes passed in by every caller.

</details>

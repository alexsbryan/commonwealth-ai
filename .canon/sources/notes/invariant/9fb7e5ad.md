# Any crate name derived from a FILE PATH must be filtered against the actual cargo workspace member list before being passed to cargo -p.

Any crate name derived from a FILE PATH must be filtered against the actual cargo workspace member list before being passed to `cargo -p`.

A directory can hold a `[package]` manifest and still sit outside the workspace — `sovereign-mobile` (standalone Tauri app) is the live example in commonwealth-ai. `cargo test -p sovereign-mobile` fails with `error: package ID specification 'sovereign-mobile' did not match any packages`, and that error aborts the ENTIRE run rather than skipping the one crate.

This bit `scripts/sovereign-test.sh`: its `crate_for_path` walks up to the nearest `[package]` manifest, which is the right answer for "who owns this file" but the wrong answer for "what can I pass to -p". The `--changed` path had this latent since it was written; it only surfaced 2026-07-24 when `--filter` auto-scoping started deriving crates from a wider grep and hit sovereign-mobile.

Fix in place: a `keep_members()` helper resolves workspace members via `cargo metadata --no-deps` and filters derived lists, reporting (not silently dropping) each rejection. If the member list can't be resolved it passes everything through — a loud cargo error beats a silently narrowed test run.

Applies to any future tool that maps changed files → crates: the SCIP/atlas tooling and the lint script share the same hazard.

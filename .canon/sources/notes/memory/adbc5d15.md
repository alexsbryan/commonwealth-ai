# When working on desktop-app (Svelte/TS) code, always run npm run check + npm run test before declaring done — they are now a blocking CI…

**When working on desktop-app (Svelte/TS) code, always run `npm run check` + `npm run test` before declaring done — they are now a blocking CI gate and catch what cargo cannot**

When editing ANY code under `sovereign/crates/sovereign-desktop/src/` (Svelte
components, TS in `lib/`), the definition of done includes BOTH, run from that
crate dir:

- `npm run check` — `svelte-check --tsconfig ./tsconfig.json --fail-on-warnings`
- `npm run test` — `vitest run` (jsdom component + unit tests)

Why: the Rust workspace `cargo check`/`cargo test` gates are blind to the
webview surface. svelte-check catches Svelte/TS type + template errors; vitest
renders components under jsdom and so catches RUNTIME render faults that no
static checker can — the class of bug that motivated this
([[invariant_svelte_each_key_duplicate_aborts_render]]: an `atom_id`-keyed
`{#each}` aborted AtomDetail's whole render and looked like an infinite
spinner). svelte-check would NEVER have caught it; a vitest render test does.

How to apply: run both after desktop edits, before saying "done." They're
fast — check ~1s warm, vitest ~2.5s for the whole suite (241 tests). Both run
via Bash directly (Node toolchain, no contention with the Rust watcher). As of
2026-07-14 they are also a blocking CI gate (`desktop-frontend` job in
`.github/workflows/ci.yml`), so a regression that slips locally will red-X the
PR anyway — but catch it locally first. The tree is warning-clean, so any
svelte-check warning you see is your own change. When you add a component,
add a vitest render test for it (convention: `vi.mock("../../api", ...)` +
`render` from `@testing-library/svelte`; see
`src/lib/components/atlas/AtomDetail.test.ts` and `library/ConflictsPanel.test.ts`).

---

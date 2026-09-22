# browser-dial — the ralph queue

Protocol: `ralph/next/browser-dial/PROMPT.md`. Campaign:
`quality/campaigns/browser-dial.toml` (two `bd-*` bars, pre-registered
2026-09-22 before any row ran). Order: `ralph/next/browser-dial/order.md`
— its Demo section is the DoD, its Predictions section is what the audit
reads the diff against, and its Less section is what every row reports
having reused.

Pointer keys: **O** = `ralph/next/browser-dial/order.md` · **C** =
`quality/campaigns/browser-dial.toml` (floors, goodharts, derives_from) ·
**TL** = `docs/THE_LINK.md` · **DEC** = `ralph/DECISIONS.md` the-link-3
(the entry this campaign corrects) · **W** =
`docs/RING_APP_LIBRARY.md:360-380,546-553` (the isolated-copy wasm recipe
and the browser claim) · **IA** = `sovereign-mesh/src/iroh_access.rs` (the
iroh plane the dial arrives on) · **M** = `scripts/ring-room-demo.sh` (the
room the dial runs inside; READ-ONLY here).

Status: `[x]` done · `[~]` in progress · `[ ]` pending. Work the first
`[ ]` row whose dependencies are all `[x]`. `HUMAN-` rows are marked by the
operator only. Serial (cargo is one worker at a time), in place.

`scripts/ralph-mark.sh <unit> <sha> ralph/next/browser-dial/STATE.md` — the third argument is THIS file.

## Rows

- [x] REVIEW-build-browser-dial-inventory a1cb78f60 — depends [] — MEASURE the tree for every premise in O §Premises and CORRECT nothing but the record. FIRST act: stamp `<BASE>` into O §Predictions (`git rev-parse HEAD`). Name in the commit: (i) the probe reproduced under `target/` at iroh =1.0.2 for wasm32-unknown-unknown with the Cargo.toml preserved, outcome verbatim, and the conditions named (toolchain 1.95.0 + components, target std, the toolbox's clang — a build fact without its instrument is a premise, ARCH 7); (ii) the DECISIONS correction entry: the Mac's tl-3 (b) outcome verbatim BESIDE this reproduction, both instruments named, never replaced — bar C `bd-wasm-build-reproduced` clauses (a)-(c); (iii) whether the daemon's iroh plane admits a GUEST at all — which alpn/endpoint a guest dial arrives on and what the door's auth path does with a guest bearer there, read from IA and the door's code, outcome verbatim either way; (iv) which browser engine the page can run in from the demo's existing containers (a headless chromium the image set can carry, or a node with native WebSocket — smallest first, §11; none is a stop, not a new fleet); (v) W's recipe re-read for what the page build copies — read: O; C; DEC; W; IA; M — check: CLEAN; DOCS(DECISIONS correction entry)
- [x] bd-1-browser-dials-relay ff6d8e561 — depends [REVIEW-build-browser-dial-inventory] — **The dial, measured: worked with the layer that answered named, or failed with the failing layer's verbatim error.** REUSE: the room (M, RING_ROOM_TOPOLOGY=room, read-only — the dial rides it, the demo script changes not at all), the phone container pattern for the browser engine the inventory named, W's isolated-copy recipe for the page build at the LOCKED iroh, the demo's grant machinery (mint, bearer), tl-3's measurement shape. ADD: one page under `target/` (never committed) that dials the live node's guest channel through the relay the daemon already uses and makes one grant-scoped `GET /v1/rail/log`; the DECISIONS entry (ledger line + appendix: every layer named with its verbatim outcome — relay connect, TLS/alpn, wasm bindgen, guest-channel admission; what would falsify each). The honest negative PASSES (the-link D3, inherited; bar C `bd-browser-dials-relay` clause (c)). ZERO product diff — clause (d); a needed edit is a §6 stop — read: O; C; W; IA; M — check: CLEAN; DOCS(DECISIONS dial entry); PLANT(none — a measurement row; the falsifier is the audit's reading of the diff)
- [ ] REVIEW-audit-browser-dial — depends [bd-1-browser-dials-relay] — TESTALL and PREPUSH; read `git log` since `<BASE>` against ARCH's twelve; THE FALSIFIER'S READING: `git diff --numstat <BASE>..HEAD` per crate, tests apart, set beside O §Predictions line by line — every crate over its predicted count and every crate changed the prediction did not name is a FINDING; `git diff --stat <BASE>..HEAD -- sovereign/crates commonwealth/crates` is EMPTY or it is the audit's loudest finding; report what each row said it REUSED (O §Less). Record under a `browser-dial` heading in `ralph/REVIEW_FINDINGS.md` — read: `sovereign/ARCH_PRINCIPLES.md`; O §Predictions, §Less — check: TESTALL; PREPUSH

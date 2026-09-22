# browser-dial — the deferred clause, measured

*Opened 2026-09-22 by operator direction in the the-link handoff session:
"queue it — this is a critical piece of the demo." It is the-link's tl-3
clause (c), which never ran because clause (b) was recorded as a build
failure that does not reproduce on the Halo.*

## Demo

The magic moment, whole: a person with a plain mobile browser, on a network
the daemon has never seen — no shared LAN, no domain, no tunnel, no tailnet
of ours — opens the guest link, and the page itself dials the daemon over
iroh's relay and makes one grant-scoped call. The relay is iroh's (or ours,
self-hosted — never the operator's home network exposed).

**This order's definition of done is the MEASUREMENT, not the moment**: the
dial becomes a fact on the record — it worked, with the layer that answered
named; or it failed, with the failing layer named verbatim. Either outcome
closes the order. An honest negative passes (the-link D3, inherited).

Wiring the dial into `scripts/ring-room-demo.sh` as a standing leg is a
FOLLOW-ON order, opened only if this one measures WORKED.

## Premises (what the inventory row verifies)

1. iroh 1.0.2 — the LOCKED pin — builds for wasm32-unknown-unknown on the
   Halo toolbox: probed 2026-09-22 in the handoff session, `Finished dev
   profile … 20.17s` with `ring v0.17.14` in the tree, rustc 1.95.0, target
   std installed. The probe dirs were /tmp (gone); the inventory row
   reproduces them under `target/` with every condition named.
2. The Mac-side tl-3 clause (b) outcome ("ring v0.17.14 … No available
   targets are compatible with triple wasm32-unknown-unknown") does NOT
   reproduce here; ring needs a C compiler even for wasm, and the error
   text is ring's cc-mapping refusal — a host fact, not a pin fact.
3. The guest link carries the `iroh=` dial string (the-link D2, landed,
   tl-2-link-carries-its-couriers) and the daemon serves grant-scoped rail
   reads over HTTP (the room demo's phone proved this every run).
4. UNKNOWN — the inventory row answers it: does the daemon's iroh plane
   admit a GUEST at all? Peers dial peers; the phone used plain HTTP. Which
   alpn/endpoint a guest dial would arrive on, and what the door does with
   a guest bearer on that plane, is read from `iroh_access.rs` and the
   door's auth path before any page is built.
5. UNKNOWN — where the page runs: wasm iroh needs a browser engine's
   WebSocket/WebRTC. The demo's phone is `node:20` fetch-only. Candidates:
   a headless chromium container on the room network, or a node with native
   WebSocket — decided by inventory, smallest first.

## Decisions

- D1–D4 are the-link's, closed and not ours. This order inherits D3's
  shape verbatim: the bar passes on honest measurement, including an
  honest negative.
- The ledger correction to tl-3 (b) lands in THIS campaign's inventory row
  as a DECISIONS entry that carries BOTH outcomes with their instruments
  (Mac verbatim failure, Halo verbatim success + conditions). The old
  record is never replaced — the delta is the finding.

## Predictions (the audit reads the diff against these, line by line)

- sovereign/crates: +0. commonwealth/crates: +0. scripts/: +0.
  A row that needs a product edit has found the order's exit condition,
  not its next step.
- quality/campaigns/browser-dial.toml: +1 file (pre-registered before any
  row ran).
- ralph/next/browser-dial/: +4 files (PROMPT, STATE, CHARTER, this order's
  copy).
- ralph/DECISIONS.md: +2 entries (the build correction; the dial outcome).
- `target/` probe + page artifacts: never committed.
- BASE: `<stamped by the inventory row's FIRST act — git rev-parse HEAD>`

## Less (what each row reports having reused)

The room (`RING_ROOM_TOPOLOGY=room`), the phone container pattern, W's
isolated-copy wasm recipe (`docs/RING_APP_LIBRARY.md:360-380,546-553`),
tl-3's measurement shape (numbers, verbatim outcomes, falsifiers, zero
product diff), and the demo's grant machinery (mint, QR, bearer).

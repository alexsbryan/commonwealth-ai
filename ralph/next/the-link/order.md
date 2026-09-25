# the-link — order

Design source: `docs/THE_LINK.md` (the lean tech design, de-risked
2026-09-21). This order builds its node-side half and measures its one open
question. Campaign: `quality/campaigns/the-link.toml` (`tl-*` bars,
pre-registered before any row runs). Regression sets, never edited here:
`quality/campaigns/ring-room.toml` (six rr-2 bars) and
`quality/campaigns/ring-guest.toml` (five rg bars).

## Objective

THE_LINK's rung 1 exists on real nodes, the link grammar carries its
couriers, and the dial's one unknown is a measured number instead of a
shrug.

**Done when:** `RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict
all` reads PASSED on every `tl-*` bar and on all six rr-2 and five rg bars,
twice from cold; `git diff --stat <BASE>..HEAD -- commonwealth/crates/
commonwealth-rail-core` is empty; and the dial report exists with numbers,
whatever they say.

**Not worth continuing if:** the checkpoint needs a `commonwealth-rail-core`
edit to verify (the design's whole claim is that shipped exports suffice —
invariant note `c3fed9c3`), or the export verb grows any surface beyond one
route, one verb and one document.

## Demo

1. During the offline leg's cut, the wall exports a checkpoint of the doc's
   journal. After the heal, the Halo verifies that file cold: marks printed,
   exit 0, and the export's timestamp sits strictly inside the cut window.
   Two corrupted copies (a flipped byte, a truncated last line) and one
   forked copy are refused by name, by the same verb, in the same run.
2. A guest link built with `at=` marks and an `iroh=` dial string round-trips
   through builder, parser and QR; a link without them is byte-identical to
   what today's builder writes.
3. `ralph/DECISIONS.md` carries the dial entry: rail-core wasm size in
   bytes, iroh wasm build outcome at the locked version, and — if it builds —
   one dial attempt's verdict. No product code gates on any of it.

## Premises (the inventory row verifies or corrects each, in place)

- **P1** VERIFIED + CORRECTED by the inventory (2026-09-21): a ring's journal
  is append-only JSONL at `<root>/rings/<ns>/ring_oplog.jsonl` and its roster
  at `<root>/rings/<ns>/roster.json` (`commonwealth/BOUNDARY.md:133`); the
  daemon enumerates namespaces from disk through the rail handle
  (`sovereign-mesh/src/ring_sync.rs:224`). The one site an export route reads
  both from: `rail.roster(&journal)` (`commonwealth-rail/src/lib.rs:238` —
  Registered/Default answerers derive from mesh membership, the File answerer
  reads `roster.json` via `journal.roster_file()`, journal.rs:498) plus ONE
  `journal.read()` (`commonwealth-rail/src/journal.rs:63`) — the exact pair
  the append/log path runs (`routes_rail.rs:494-504`).
- **P2** `admit`, `digest` and `ops_missing_from` are public exports
  (`commonwealth-rail-core/src/lib.rs:91-95`; `admit.rs:307`,
  `sync.rs:133,176`) and the four verification steps of `docs/THE_LINK.md`
  §"The checkpoint, specified" run on them with zero rail-core edits.
  VERIFIED (inventory 2026-09-21): `Ed25519Verifier` is rail-core's own
  (lib.rs:531); `Op`/`SkippedLine` re-exported at lib.rs:102; admission runs
  signature, id-derivation, roster, sequence, fork (`RailGap::SequenceFork`,
  admit.rs:109) and void rules.
- **P3** The guest-link builder and parser are
  `commonwealth/crates/commonwealth-discovery/src/deep_link.rs:195-259`,
  percent-encoding in file. CORRECTED by the inventory (2026-09-21): ONE
  production caller of `build_https_guest_link` (`mesh_guest.rs:616`);
  `mesh_guest_link.rs:72` and `deep_link.rs:815` are tests. The encoder is
  `percent_encode` (deep_link.rs:492-503, unreserved set
  `ALPHA DIGIT - . _ ~`); `parse_query_params` (:426-436) neither lowercases
  nor trims — values are percent-decoded verbatim (`+`→space in
  `percent_decode`, which the builder compensates only for `s=`). The
  fragment rule is structural: the parser splits at `#` first (:245) and
  reads nothing but the fragment.
- **P4** The offline leg already records `cut_at`, `heal_at`, `converged_s`
  and polls both journals byte-equal (`docs/RING_ROOM_RUN_OF_SHOW.md:136-144`)
  — the export hooks between the cut and the reconnect. VERIFIED (inventory
  2026-09-21): cut_at at ring-room-demo.sh:1426, heal_at :1435, recorded into
  `room-offline.json` :1448-1457 — the export slots between :1426 and :1435.
- **P5** rail-core compiles for wasm32 only in an isolated copy today
  (`docs/RING_APP_LIBRARY.md:360-380`, measured 2026-09-20 — workspace-hack
  carries tokio; `getrandom` needs the wasm_js backend). MEASURED by the
  inventory (2026-09-21): the isolated copy builds clean AND the cdylib
  sizes at 415,140 bytes (probe.wasm, sha256 9a07541e…9831d7, rustc 1.95.0,
  release-default opt-level=3, no wasm-opt) — bar C clause (a)'s number.
  iroh at the locked version CLAIMED browser relay-only builds — now
  MEASURED: iroh 1.0.2 does NOT build for wasm32-unknown-unknown; the
  failing layer is `ring v0.17.14`'s build script ("No available targets are
  compatible with triple wasm32-unknown-unknown"). `tl-3` re-measures both
  and records them in `ralph/DECISIONS.md`.
- **P6** The demo's verdict printer reads floors from campaign files and was
  last pointed at two of them in ONE `verdict all` (ring-guest's instrument
  row) — it takes a third without a new script. VERIFIED (inventory
  2026-09-21): `ROOM_CAMPAIGN`/`GUEST_CAMPAIGN` at ring-room-demo.sh:83,87;
  the python takes both as argv (:1502) and folds them into one bars map
  (:1506-1509); `verdict` validates ids against both (:2210-2218).
- **P7** `svrn ring <sub>` dispatch lives in `sovereign-cli-llm/src/ring_cmd/`
  and `ring log` is the read-only verb whose shape a checkpoint verb copies.
  VERIFIED (inventory 2026-09-21): flat `match args.first()` (mod.rs:68-105);
  `run_log` :654-715 through `sovereign_cli_shared::rail::rail_log`
  (re-exported mod.rs:194) — the ONE daemon client a checkpoint verb shares.

## Decisions (operator, 2026-09-21 — not the queue's to reopen)

- **D1** `at=` carries the digest marks (`<actor-hex>:<n>` pairs), never the
  payload — a QR holds ~3KB and the checkpoint document is fetchable data.
- **D2** The courier field reuses the join link's `iroh=` dial string; no
  `relay=` list is invented.
- **D3** The dial row MEASURES and never gates: its bar passes on honest
  numbers, including an honest "iroh does not build for wasm at this pin."
- **D4** Verification is node-side this campaign. The in-page verifier waits
  on `tl-3`'s size number, and is not ordered here.

## Predictions

`<BASE>` = `71bfde4594a23bebdbd41fcee4f0ec83095d3e09`, stamped by the
inventory row 2026-09-21. Lines, tests apart:

| crate / path | predicted |
|---|---|
| `commonwealth-discovery` | +55 (params, round-trips, refusals) |
| `sovereign-daemon` | +90 (export route, document, tests) |
| `sovereign-cli-llm` | +130 (two verbs, verdict text, tests) |
| `scripts/ring-room-demo.sh` | +110 (offline-leg hooks, tamper legs, printer's third campaign) |
| `commonwealth-rail` | 0, unless the inventory names a reader gap — then ≤15, named |
| `commonwealth-rail-core` | **0 — a diff here is the audit's loudest finding** |
| `sovereign-grants`, templates, guest surface | 0 |

## Less

What every row reports having reused: `admit`/`digest`/`ops_missing_from`
(shipped exports — the campaign exists to prove they suffice);
`deep_link`'s builder/parser and its fragment rule; `scripts/ring-room-demo.sh`'s
legs, seal, census and verdict printer; `ring_room`'s cut/heal recording; the
`ring log` verb shape; `fast_qr`'s existing SVG path. Nothing new is minted
that one of those already does.

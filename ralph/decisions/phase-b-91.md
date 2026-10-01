<!-- ledger -->

**phase-b-91 · 2026-10-01 · pb-mesh-exit-mesh · director, ruling how svrn's storage budget reaches cw-rails' merge (phase-b-83 (1)); amends the row's (1) bullet** — this commit
- Needed: after (1) the daemon measures nothing, so its declaration must carry the budget, and `NodeCapabilities` has no field for it. On the existing fields zero means "not declared" under (B), and zero is also a real answer: both clamps floor to whole GiB (sovereign-mesh capabilities.rs:138 `(remaining / 1_073_741_824) as u32`, :341 the same before `as f32`), so any budget under 1 GiB left declares 0. Checked at 8f9919aef.
- Chose: the package's option 1. `oicp_types::capabilities::NodeCapabilities` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub storage_remaining_bytes: Option<u64>`, documented as a registrant's declaration that cw-rails consumes and never gossips. The daemon's `claims_source` sets it from `storage_remaining_bytes()` (sovereign-daemon state.rs:1755). `merge_declared` (commonwealth-rails origins.rs:187) clamps the measured `hardware.free_storage_gb` and `available.free_storage_gb` to the smallest declared ceiling with the existing whole-GiB rounding, before live resources, beside (B)'s VRAM rule, and never copies the field onto the self row. `Some(0)` clamps to zero; only `None` means "no budget declared". The 26 `NodeCapabilities {` literals in 17 files gain `storage_remaining_bytes: None`.
- Because:
  - Principle 11. The declaration channel already exists end to end and is re-read live: `ClaimsSource` is called at every register AND every renew (sovereign-turn-client rails_origins.rs:141-143, :167-170), and `renew_origin` posts `claims` as `NodeCapabilities` (rails_origins.rs:72-82). The budget shrinks as storage is used, so it must ride the renew. Option 2 (`OriginRegistration`) missed this: the renew body carries no registration, so it would need a new renew field, a widened `ClaimsSource` and a second `declared_*` accessor in commonwealth-media (origins.rs:395) — three surfaces to save nine literals.
  - Principle 6. An `Option` keeps "not declared" apart from "exhausted"; option 3 would advertise the full measured disk exactly when the budget is spent.
  - Principle 12. The field is svrn's answer about itself; cw-rails applies it to what it measured, the shape (B) already set for serve's VRAM. `OriginRegistration.claims`' doc ("gossiped as-is") is already false after (B) and is corrected in the same commit as the field.
  - No edge, no exception, no ratchet; peers' wire is unchanged because the self row always carries `None`.
- REVIEW-AFTER: pb-mesh-exit-mesh's landing. Falsified if `oicp-types/tests/mesh_records_wire_fixture.json` changes, if this host's self row in ~/.commonwealth-rails/mesh.json advertises a different `free_storage_gb` than before the landing with the same budget, or if a cw-rails test with a declared `Some(0)` does not advertise zero free storage.

<!-- appendix -->

## phase-b-91 · 2026-10-01 — svrn's storage budget rides `NodeCapabilities.storage_remaining_bytes` into `merge_declared`

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.md of 2026-10-01 (worker, progress 3 on the `[~]` row). Every fact it relied on was reproduced at 8f9919aef: the clamp and its rounding (capabilities.rs:135-151, :334-347), `media_available: Option<f32>` as the absence-shaped precedent (oicp-types capabilities.rs:89-99), `merge_declared` taking the first hardware-reporting declaration wholesale (origins.rs:187-211), its one caller (gossip.rs:183, over `declared_claims()` at :161), 27 `NodeCapabilities {` hits in 17 files (26 literals plus the definition), 15 `OriginRegistration {` hits (14 literals).

The package priced option 2 at 14 literals plus `merge_declared` plumbing. It did not price the renew path: `keep_registered_declaring` refreshes only `registration.claims` from the `ClaimsSource` at register and sends `declared` (a `NodeCapabilities`) on every renew; a ceiling on `OriginRegistration` would be frozen at register time while the budget it describes falls with every stored shard. That decided it.

The tests the row already names cover it: the cw-rails pinning test for hardware and clamped storage gains a `Some(0)` case and a no-declaration case, and asserts the self row's `storage_remaining_bytes` is `None`. PLANT: drop the clamp in `merge_declared`, and that test goes red.

</details>

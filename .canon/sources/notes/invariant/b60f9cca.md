# A FILE NOTHING DECLARES IS INVISIBLE TO EVERY FILE-BASED AUDIT, BECAUSE AUDITS READ FILES AND THE COMPILER READS MODULES. Found 2026-09-01…

A FILE NOTHING DECLARES IS INVISIBLE TO EVERY FILE-BASED AUDIT, BECAUSE AUDITS READ FILES AND THE COMPILER READS MODULES. Found 2026-09-01 in the ontology-v1 P0 crate split.

WHAT HAPPENED: splitting `inference_client.rs` into `mod.rs` / `wire.rs` / `discovery.rs`, the splitter's `mod.rs` write used a conditional `.replace()` whose fallback silently emitted the UNMODIFIED text — so `mod wire;` and `mod discovery;` were never written. Both files existed on disk, were rustfmt'd, passed the leave-behind-imports audit AND the visibility audit, and were absent from the build entirely.

THE TELL, and why it reads as the wrong bug: the compiler said `E0599: no method named complete_openai_compatible found for &DaemonInferenceClient` — which looks like a VISIBILITY problem and sends you to add `pub`. The methods were not private; they did not exist, because their module was never compiled. Chasing the visibility reading would have widened a wire format's internals onto a crate's public surface to fix a missing `mod` line.

THE GUARD: a reachability pass that walks `mod` declarations from the crate root and asserts every `.rs` under `src/` is reached. It found exactly those two files and now reports 25/25. Cheap, and it is the only check in the family that operates on the same graph the compiler does.

RELATED, SAME SESSION, SAME WORKER — privacy is DIRECTIONAL and the naive check gets it backwards. An item private to module `m` is visible in `m` AND ITS DESCENDANTS, never in its parent. So:
  - `wire.rs` touching `DaemonInferenceClient`'s private fields is LEGAL (`wire` is a descendant of the module that declares them);
  - `build/mod.rs` reading `Plan.enabled` is ILLEGAL (`plan` is mod.rs's CHILD).
A first sweep written as "flag any cross-FILE access" flagged the legal direction and missed the illegal one — worse than no check. Model the parent/child relationship, not "different file". Fixes are `pub(super)`, never bare `pub`: bare `pub` on a split-out internal puts a plan's or a wire format's guts on the crate's public surface, which is the opposite of what a crate split is for.

Third instrument failure from one worker in one night, each in a different direction: the import scanner FABRICATED findings (344d10d1), the splitter SILENTLY DROPPED 444 lines (08fe0db3), and this audit family reported PASS on files the build never saw. All three were caught by validating the instrument against a known answer (ARCH §18.4), never by reading its output.

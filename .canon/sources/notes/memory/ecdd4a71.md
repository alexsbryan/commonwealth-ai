# Daemon's cancel endpoint now requires confirm_wipe: true and /pause is the non-destructive alternative; SIGTERM is no longer the only safe…

**Daemon's cancel endpoint now requires `confirm_wipe: true` and `/pause` is the non-destructive alternative; SIGTERM is no longer the only safe stop**

`POST /internal/corpus/cancel {"corpus_id": "X"}` on the daemon's internal port (9742) is destructive — it calls `remove_corpus_everything(X)` and deletes `chunks.lance/` contents + resets `_corpus_meta.json`. Since 2026-04-25 it requires `"confirm_wipe": true` in the body or it returns 400. The non-destructive variant is `POST /internal/corpus/pause`.

Why: I called cancel during an active wikipedia ingest thinking it was a pause and destroyed ~15 days of embed work (15.2M chunks, 99 GB partition). The name "cancel" naturally reads as "stop the current run", but the endpoint was for removing a corpus entirely. The fix landed as: (1) `/internal/corpus/pause` — signal cancel + wait for task exit, no wipe, on-disk state preserved so re-installing resumes from `committed_iter_pos`; (2) `/internal/corpus/cancel` now requires `confirm_wipe: true` — guardrail against accidental wipes; (3) Desktop's in-progress button now reads "Pause" and calls `pause_corpus`, "Remove" still goes through `remove_corpus` with confirm.

How to apply:
- To stop an ingest while keeping data: `POST /internal/corpus/pause {"corpus_id": "X"}` — preserves chunks.lance + _corpus_meta.json. Resume by `POST /internal/corpus/install` again.
- To delete a corpus entirely: `POST /internal/corpus/cancel {"corpus_id": "X", "confirm_wipe": true}`. Without `confirm_wipe` you get a 400 with a hint pointing at /pause.
- SIGTERM on the daemon still works as a process-level pause and is the right move when you want to free the embed slot or restart with a different config — but for a single-corpus pause, the new HTTP route is cleaner.
- When writing bench utilities that share the daemon's embed slot: still applies that you should not call cancel; either use /pause, SIGTERM, or load your model in-process via a separate `llama-server` (the v5 bench did this on port 8090 — see `sovereign/bench/atlas_retrieval/embedding-comparison.md`).

Endpoint locators: `commonwealth/crates/commonwealth-api/src/routes_internal.rs` — `corpus_pause` and `corpus_cancel` handlers; both use the shared `stop_in_flight_ingest` helper. Lifecycle tests: `commonwealth/crates/commonwealth-api/tests/corpus_lifecycle.rs::install_pause_resume_lifecycle` and `install_cancel_reinstall_lifecycle` (which now also verifies the 400 guardrail).


## Index overflow (moved from MEMORY.md 2026-07-07 compaction)

- [/internal/corpus/cancel is destructive — pause is safe (2026-04-25)](feedback_corpus_cancel_is_destructive.md) — cancel now requires `confirm_wipe: true`; use /internal/corpus/pause for non-destructive stop with resume

---

# Clients hitting /v1/chat/completions must pass the actual loaded model name (gguf file stem), not a slot abstraction

Clients hitting /v1/chat/completions must pass the actual loaded model name (gguf file stem), not a slot abstraction

The Commonwealth daemon's `/v1/chat/completions` route does NOT resolve abstract slot names like `"fast"`, `"primary"`, or `"embed"` to whichever model is loaded in that slot. Default `default_aliases.toml` has no such entries.

The 4-priority resolver in `commonwealth-api/src/routes_inference.rs`:
1. Local in-process inference (when `inference_provider` is wired)
2. Exact name match (case-insensitive) against `ModelInfo.name` — which is the gguf file stem (`Qwen3.5-9B.Q8_0`, not `"fast"`)
3. Alias resolution via `default_aliases.toml`
4. Default model from inference plan

`register_local_model_slots` in `sovereign-mesh/src/daemon.rs` registers each slot under `path.file_name().trim_end_matches(".gguf")` — so callers must pass the file stem.

Why: I lost an hour debugging OCR cleanup 503s ("No models are currently loaded on the mesh") because the OCR module had a misleading comment claiming the daemon resolves `"fast"` to the fast slot. It doesn't — that was a design assumption that was never implemented.

How to apply:
- When writing a client that hits `/v1/chat/completions`, get the actual loaded model name from config (file stem of `model_path`), don't hardcode `"fast"` / `"primary"` / etc.
- If you want abstract slot routing, the cleanest path is OICP `latency_class` in the request — but that requires the daemon's plan to be populated and at least one model whose synthesized claim scores against the request.
- When the desktop is in `BootstrapMode::Attach`, OCR cleanup hits the CLI daemon, not an embedded one. The CLI daemon doesn't have `local_inference` wired (Priority 0 fires only for embedded daemons running an `EmbeddedLlamaCpp`).
- If no daemon endpoint can serve the cleanup, fall back to raw OCR text — never throw away signal because the polish pass failed.


## Index overflow (moved from MEMORY.md 2026-07-07 compaction)

- [Daemon doesn't auto-resolve "fast"/"primary" slot aliases](invariant_daemon_no_slot_aliases.md) — /v1/chat/completions resolves by gguf file stem, not slot abstraction. Pass actual model name from config, not "fast".

---

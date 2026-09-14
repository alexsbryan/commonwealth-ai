# Build and run DEBUG binaries, not --release, for this project's iterate/test/run cycles

Build and run DEBUG binaries, not --release, for this project's iterate/test/run cycles

Use debug builds, never `--release`, for build/test/run iteration in
commonwealth-ai. Run the debug binaries directly from `target/debug/<bin>`
(e.g. `target/debug/sovereign-cli-llm recipe-agent …`).

Symlink is DEBUG now (2026-06-10): `~/.local/bin/sovereign` →
`target/debug/sovereign-cli` (user re-confirmed "don't use release"; CLAUDE.md
still says release — the symlink override wins). GOTCHA: the `tools` verb is
feature-gated — the dispatcher must be built `cargo build -p sovereign-cli
--features dev-tools` or every `sovereign tools …` call fails with "not in the
default build". Siblings (cli-dev/cli-llm/cli-daemon) build plain.

Why: release builds take many minutes and the user iterates constantly;
debug compiles are far faster and already warm from `cargo check`. The user
called this out explicitly (2026-06-03) after a `cargo build --release -p
sovereign-cli-llm` wasted time.

How to apply: `cargo build -p <crate>` (debug). Invoke `target/debug/<bin>`
directly. The running daemon's binary version doesn't matter for client-side
work (e.g. the recipe-agent live-trial reads its schema/tools client-side; the
daemon only serves inference). Only rebuild the daemon binary when a daemon-side
code change must land. See [[reference_daemon_restart_lwcr.md]].

Exception — OCR needs release. PaddleOCR is only compiled into the
release build, so any path that actually runs OCR (the `described_asset`
PDF-scan extractor — e.g. the UAP Level 2 layer) must use the release binary /
daemon. Debug is fine for everything that doesn't OCR (JSONL/text/HTML, the L1
metadata path). See [[project_paddleocr_bakeoff_2026_05_27.md]].

Caveat — schema/grammar inference can trip the 300s deadline (cause UNPROVEN).
Observed once: a `recipe_write_structured` (JSON-Schema-constrained) request on
the debug daemon returned 503 "inference deadline exceeded after 300s …
pathological JSON-Schema mask state"; the same request on a release daemon
finished fast. I previously wrote this up as "debug mask is too slow" — that
was an overclaim (challenged 2026-06-03, conceded). The evidence was N=1
each, not isolated (build was only one of several differences), and the code
itself attributes the timeout to a *pathological mask STATE*, not a build type:
`model_slot.rs:530` — "Defends against pathological JSON-Schema mask states where
the per-token mask computation degrades from ~25 ms/token to 100s of ms/token …
Default 300s — generous enough that any legitimate Slow-slot Phase-1 call
(~60-160s worst-case under heavy grammar) finishes." So the trigger is most
likely the schema/input shape (possibly intermittent), and a debug build may
*amplify* an already-slow mask — but "debug → guaranteed 503" is not established.
How to apply: if a schema/grammar request 503s on the deadline, retrying on
a release daemon is a reasonable mitigation, but don't assert the build *caused*
it. Knob: `SOVEREIGN_INFERENCE_TIMEOUT_SECS` (raise it to test the slow-vs-stuck
question). Plain chat / embed / the ingest-extract-chunk pipeline have no
per-token mask and are unaffected; `corpus install` (F2 path) runs fine on debug.
Note `~/.local/bin/sovereign` → `target/debug/sovereign-cli`, so `sovereign
daemon start` launches the DEBUG daemon.


## Index overflow (moved from MEMORY.md 2026-07-07 compaction)

- [Use debug builds, not release](feedback_use_debug_builds.md) — build/run `target/debug/<bin>` directly for iteration; `--release` wastes minutes. Daemon binary version irrelevant for client-side work (live-trial reads schema/tools client-side).

---

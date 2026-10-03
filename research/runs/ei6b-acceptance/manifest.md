# ei6b-acceptance — item 5: the whole-game check on the quirks change

Staged 2026-09-08 by ei-6b. **Pre-registered before the run.**

## The question

ei-6b changed what `corpus-mcp` sends to an embedding endpoint on both sides:
documents now carry the family's EOS marker, and the atlas seed table is built
with the **query-side** embedder rather than the document-side one — the
contract `build_with_progress_with_embedder` always stated
(`sovereign/crates/sovereign-enrichment-build/src/build/mod.rs:41-48`) and
which this crate had been violating since it grew an atlas.

Cosines say that is right (+0.128 mean against a daemon-built seed table, note
3588fa45). Cosines are not the product. This run asks the whole-game question:
**is the corpus a third party BUILDS with the changed code as good as the one
our daemon builds?**

## Where the "before" comes from — one run, not two

`acceptance.sh`'s recall leg scores `sovereign-recipes/wessex-hoard/truth.json`
through `scripts/truth-recall.py` — the ONE scorer, ei-3c's, not a copy — over
**two** atlases in the same invocation:

- the daemon-built `wessex-hoard` CONTROL, and
- the atlas this run just built from two bare `llama-server` processes.

**The control is the before.** Running main a second time would cost another
~80 minutes and produce numbers from a *different scorer* — `runs/ei5b-stage2`
(2026-09-05) predates ei-3c's fix, and the campaign's own trap note says the
old path compared raw yield rather than truth.json recall. Comparing across
that boundary is worse evidence, not better. The ei5b-stage2 rows are cited in
the report for orientation only, flagged as a different scorer.

## The bar — not ours, and not tuned here

`acceptance.sh` FAILS the run itself if the bare-endpoint atlas is below the
daemon-built control on any `truth.json` bar
(`acceptance: FAIL - recall below the daemon-built control on: ...`). We report
its verdict; we do not compute one. The order's bar is "unchanged or better",
which is exactly that gate.

## Pre-registered outcomes

| marker | meaning |
|---|---|
| `VERDICT-ACCEPTANCE-PASSED` | exit 0 — every mechanism assertion held AND recall met the control on every bar |
| `VERDICT-RECALL-BELOW-CONTROL` | the change made the third-party-built atlas worse; a defect in this order |
| `VERDICT-RECALL-COULD-NOT-JUDGE` | the script's own named outcome when the ingested corpus is not the one `truth.json` describes — reported as such, never read as a pass |
| `VERDICT-ACCEPTANCE-FAILED-ELSEWHERE` | something outside the recall leg broke; a verdict about the run, not about recall |

A `could-not-judge` is not a pass and is not a failure. It is the third verdict
and it gets its own marker (ARCH §18.2).

## Controls and traps

- `wessex-hoard` is READ as the control and **never written**. The build lands
  in a NEW id, `wessex-hoard-bare-ei6b`, and preflight REFUSES if that id
  already exists — a leftover would make the ingest *resume* rather than
  rebuild, and the run would measure a hybrid of two code paths.
- **Stale-binary check.** The run refuses if anything under `corpus-mcp/src`
  is newer than `target/debug/corpus-mcp`. A stale binary would measure main
  and report it as this branch — the trap `runs/ei6-restore-probe` added after
  learning it the hard way.
- `CHAT_GGUF` is `Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf`, **the same model the
  2026-09-05 candidate used**, so any difference is the code path and not the
  model.
- All preflight — both GGUFs, `llama-server`, `jq`/`python3`/`curl`, the
  scorer, `truth.json`, the control corpus, the free id — runs BEFORE the
  ~80-minute ingest.
- Memory gate wants MemAvailable >= 40 G, because this run starts a **second**
  `llama-server` holding a 35B beside the embedder.

## Cost

`runs/ei5b-stage2` measured the ingest leg at **4773 s (~80 min)** with this
same chat model. Budget ~90-100 min including the build, the mechanism
assertions and both recall passes. Terminal `DONE` marker on
SIGTERM/INT/HUP, and a partial acceptance log is preserved on a kill.

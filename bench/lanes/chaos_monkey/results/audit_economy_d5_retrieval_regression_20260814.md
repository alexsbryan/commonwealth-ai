# D5 — the retrieval regression: our own arms carried the writer that slowed them (order audit-economy, amended)

2026-08-14. Measurement-first per directive 6fdc5796/amendment; all evidence
from existing instruments (daemon logs incl. two rotated segments, the smoke
census, the desktop process log, Lance version manifests on disk). One code
change (the fold hook) + one protocol change ship with this; the root-cause
defect is filed, not hidden.

## The term, named

All three inflated surfaces are ONE term: **hybrid search over the
`wikipedia` Lance dataset while it carries thousands of freshly-written,
unfolded fragment rows** — lancedb serves a query by running the index over
indexed data and FLAT-SCANNING everything appended since (the b34c9673
mechanism). Wikipedia is the only corpus in the ~40-corpus fan-out that was
decayed and the only one whose search is expensive: in the smoke's retrieval
fan-outs every other corpus answered in 1-173ms while wikipedia answered in
394-1777ms (daemon side) and 363-663ms rising (desktop side); during the
DISCARDED first smoke attempt, mid-burst under memory pressure, desktop
wikipedia searches ran 9.1-15.7s each.

## The cause chain, cited

1. **`WikipediaNewsworthyWatcher` re-ingests every tracked article on every
   daemon restart.** Its boot tick re-ingested the portal and re-tracked all
   links as NEW (`newsworthy.portal_ingested new_links=85 new_tracked=85`),
   then ran `initial_fetch_attempt` for each (`reindex.committed
   corpus_id="wikipedia" chunks_written=93/11/...`). Both dedupe layers were
   defeated: the tracked registry was EMPTY at boot, and every logged revid
   is 0 (`/parse/revid` extraction yields nothing), which disables the
   KV short-circuit at newsworthy_watcher.rs:970 (`observed_revid > 0`
   required). Root-cause repair is FILED (backlog: newsworthy-restate
   persistence + revid parse), not attempted here — it is M-sized inside a
   1,780-line watcher.
2. **The burst lands exactly where the arms measure.** Lance manifest mtimes
   on disk: 132 commits 03:54-04:00Z (the seat restarts the daemon to prep
   the baseline/after arms; arms ran 03:59-04:26Z) and 169 commits
   17:56-18:06Z (pre-smoke restart 17:54; smoke 18:10-18:18Z). Sweep
   telemetry brackets both: `max_unindexed=0` at 03:48 -> 13,353 folded at
   04:50; `max_unindexed=0` at 17:56 -> **17,373** folded at 18:59 (84
   fragments, 6.5GB reclaimed).
3. **The hourly sweep is healthy — its cadence is just wrong for bursts.**
   Cycle-complete lines run hourly all day (max_unindexed=0 at 05:48,
   06:48, ..., 16:48, 16:59, 17:56). A burst that lands at minute 8 of the
   hour flat-scans for ~52 minutes — which is longer than an arm.

## The three-surface constraint, satisfied

| surface | measured | explained by |
|---|---|---|
| strips baseline 4.8s -> after 7.6s (n=21 each) | same-day pair | both arms inside the 03:54 burst decay; the after-arm ran deeper into it |
| smoke retrieval stage 8.15s median | live at HEAD | 17:56-18:06 burst, unfolded through the whole smoke |
| gate claim-search ~0.8 -> ~1.2-1.4s/claim | same substrate | the desktop-process searcher reads the same decayed Lance files (attach mode) |

Post-sweep verification: CLI wikipedia hybrid search 2.86s wall x3
(healthy floor; the b34c9673 precedent's post-maintenance number was 2.96s);
sweep at 19:36 reads max_unindexed=0.

## Fixes

- **F1 (protocol, ships in the staged run dirs):** every arm-prep daemon
  restart is followed by `svrn corpus optimize wikipedia` (verb already
  exists — principle 11) and a clean-state check BEFORE the arm starts.
  Added to `runs/audit-economy-live-discipline/run.sh` (leg 0) and the
  ladder-shadow manifest; the composed after-arm inherits the same
  pre-flight. Without this, every future same-day arm re-measures the
  confound.
- **F2 (code, this commit):** the watcher folds its own writes — tick end
  calls `CorpusIndex::optimize(None)` for each corpus it committed into and
  records `folded_corpora` in the TickReport. The "anything to fold?"
  decider stays INSIDE `optimize` (its index phase self-gates,
  `skipped_as_clean`; no second floor — ARCH §10.6). Pruning remains the
  sweep's destructive decision. Pinned by
  `first_tick_fetches_pending_articles_into_parent` (negative control run:
  hook disabled -> `folded_corpora=[]` -> test failed; first-cut assertion
  on `fragments_removed` was discarded as vacuous — it could not fail).
- **F3 (filed):** the actual burst generator — tracked-registry not durable
  across restarts + revid always 0 — wastes a full 85-article re-fetch,
  re-embed and re-ingest (~17K chunks) on every daemon restart, on every
  node running the watcher. Backlog item with this evidence; corpus-engine
  update path, M.

## What this does to the order's arithmetic

The +3.3s the smoke put in the audit's non-call term and the ~3.3s of
retrieval-stage inflation were environment, not mechanism: at healthy search
prices the D2-smoke arm reads ≈ 22.1s audit median (25.39 measured − ~3.3
confound), against the D0-era 27.8 baseline that was itself measured partly
decayed (its arms sat inside the morning burst). The composed after-arm, run
under F1, measures the levers without the confound. The bar still needs D6
(ladder) and D7 (decode) — D5 removes noise and a real production tax
(every user turn within ~52 min of any daemon restart paid 2-15s extra per
wikipedia search), but it is not by itself the 16.8s.

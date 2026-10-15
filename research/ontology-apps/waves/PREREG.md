# The compiled wave reader on the journey: pre-registration, 2026-10-12, before either arm runs

Question. The compiled reader (`SOVEREIGN_READER=waves`, aa50e89cd) compiles the declaration into levels of closed
questions, particulars first, and asks each round of a level as one forced-choice wave off one document prefix. On the
journey corpus, does it serve Ward's deals at least as well as the default reader, at lower cost per document?

Population. `ward-story`: the 37 messages of Ward's tep/ and pasadena/ folders, recipe
`research/ontology-apps/ward/recipe-story.toml` (Ward's plus the company identity criterion). Gold: Ward's, tep-d1..d6 and
pasadena-d1..d8, as the frozen Ward ladder slices it to the documents a run read (`ladder_ward.measure`).

Arms. One binary, built once from the working tree and frozen with its head and diff hash; two twin corpora from the
recipe with only `[corpus] id` changed, `ward-story-r` and `ward-story-w`, so neither arm overwrites the other or the
journey's own `ward-story`. Each runs the journey's public commands (default data root, `RUST_LOG=info,
corpus_engine=debug NO_COLOR=1`, no `SOVEREIGN_WAVES_ONE_BY_ONE`): `corpus install <recipe> --wait`, `enrich init <id>
--from-corpus <id>`, `enrich extract <id> --full`, `enrich resolve <id>`. Arm R: `SOVEREIGN_READER` unset. Arm W:
`SOVEREIGN_READER=waves`. R runs first, then W, on the shared daemon in a window it is free; one run each.

Measures, per arm, from the frozen Ward ladder over a leg directory holding the twin's index and atlas and the run's
debug log:
- M1 deals served at their current stage: the ladder's hit over its sliced population, and where each other deal was
  lost (read, place, fold).
- M2 the ladder's bars: deals matched, stage served.
- M3 cost per document: extract wall seconds (the corpus's `_tokens.json`, over the documents read), questions (the
  reader's `chapter read ... calls`), requests (`/v1/chat/completions` lines).
- M4, arm W only, replay (C5): the twin's config pointed at a dead port, `enrich extract <id> --full --asker replay` and
  `enrich resolve <id> --asker replay`; the atlas's record files byte-identical to the live run's, and refusals counted
  from the Asker's own log lines (`runs/party/job.sh`'s check).
Reported, no bar: W's questions and rounds per level, and the journey's three atlas-query questions per twin once their
expected facts are written (ec, `svrn/docs/cli-contract.toml`).

Bars, fixed now.
- B1 W replays: every record file identical, no refusal.
- B2 W is not worse: on M1's hit and on each M2 bar, W >= R - 1 (one run each over at most 14 deals; a difference of one
  is inside the churn measured on Ward's stage, 2026-10-08).
- B3 W is cheaper: W's extract wall seconds per document <= 0.6 x R's.
Verdict: the wave approach is demonstrated on the journey when B1-B3 pass. B2 passing and B3 failing names transport
as the next lever (a wave restoring the document's pinned prefix across rounds, deferred); B2 failing, W's lost deals
are classed by the ladder's rung before any change.

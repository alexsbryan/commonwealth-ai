<!-- ledger -->

**phase-c-9 · 2026-10-02 · pc-deployed-turn-latency · worker** — this commit
- Needed: the row asks for the deployed node's plain-turn gap against a clean root, attributed stage by stage, with each cost named needed or removed.
- Chose: census only, no code. The gap is not the same work running slower. The clean root and the deployed node run the same plan (KnowledgeQuery, stream_knowledge_query_turn), and the clean root runs it over 0 corpora: 0 chunks, search 81 ms, no gate. The deployed node runs it over 380 installed corpora (377 searched), and that brings retrieval, a 20-chunk synthesis and a grounding gate along with it. Every stage is attributed below. One cost is neither needed nor mine to remove: 316 `dr-estate-dr-*` deep-research run corpora, searched on every turn, which contribute 0 of the 20 survivors. That one goes to the operator (ralph/next/phase-c/ctl/NEEDS_HUMAN.md).
- Because: removing those corpora is user data, and leaving them out of the unscoped fan-out is end-user behaviour the row does not state (charter, "Leave these for the operator").

<!-- appendix -->

## phase-c-9 · 2026-10-02 — the deployed turn's gap is the node's 380 corpora, attributed stage by stage

<details><summary>reasoning, evidence, package</summary>

The reading: n=3 plain turns ("In one sentence, what is a compiler?", the throughput lane's `[e2e]` question), sent with `POST /v1/conversations/{id}/messages` to the deployed node (pid 2811764, target/debug/sovereign-stock, started 2026-10-02 00:43 PDT, RUST_LOG at info for sovereign_core and corpus_engine, debug for sovereign_inference). Times are 01:03-01:05 PDT. Loads were 2.53, 2.40 and 2.93, and no cargo was running in this lane. Stage times come from the daemon's own journald lines (conmon --syslog). Raw files: target/ralph/phase-c/census/{turns.txt,turn-{1,2,3}.json,journal.txt} in the lane worktree.

| stage (s) | turn 1 | turn 2 | turn 3 | scales with | verdict |
|---|---|---|---|---|---|
| wall (client) | 62.6 | 56.9 | 35.9 | | |
| provenance total_ms | 20.0 | 33.9 | 18.2 | synth + gate only | instrument finding (below) |
| cold slot reload (fast 5.9, embed 1.6) | 7.5 | 0 | 0 | idle unload at 900 s | needed: first turn after idle |
| route (housekeep + classify) | 5.5 | 6.5 | 1.8 | corpus count via the router prompt | needed, paid on a prefix miss |
| local fan-out (377 corpora) | 7.9 | 6.1 | 6.2 | corpus count | 316 dr-estate: owed to the operator |
| atlas grounding (265 candidates) | 18.9* | 8.5 | 8.4 | atlas-bearing corpora | needed; ~7.5 s of it emits no event |
| merge + turn summary | 2.3 | 0.1 | 1.3 | | needed |
| synthesis, fast slot 4B | 13.0 | 13.7 | 10.2 | chunk count (TTFT 11.8/10.4/9.0) | needed: 20 chunks, 26k-char prompt |
| gate | 7.0 | 20.0** | 7.9 | grounded mode | needed |

\* includes the one-time `wiki atlas provider: resident store built` (9.2 s, 51,781 atoms, 2.29M edges), paid on the first turn after boot.
\** citation 3.7 + claim extraction 2.0 + one refusal retry 12.6.

The clean-root turn (target/ralph/phase-b/ship/esc/runs/throughput-1-C/daemon.log in the main tree) shows the same plan: routed KnowledgeQuery, `local_corpora={}`, `chunks_found=0 search_ms=81`, synthesis 670 ms, no gate. The lane reads 672 ms there.

Each stage, and what it scales with in the node's state:
- Router. The classify prompt embeds `installed_corpora_display()` (sovereign-contracts types/conversation.rs:438), which joins all 380 ids into one comma-separated list. The prompt is 14,240 chars on the deployed node against 4,907 on the clean root, about 7k tokens. The cost is paid only when the prefix misses. Turn 2 re-learned it in 5.4 s, and turn 3 restored it (`prefix_state: HIT ... restored_tokens=6982`) and classified in 0.5 s.
- Fan-out. 377 `KnowledgeQuery: search complete` lines in 5.1 s of wall time. Their per-corpus elapsed times sum to 16.2 s: 316 dr-estate-dr-* corpora account for 6.4 s and 61 others for 9.8 s. No dr-estate corpus is among the 20 survivors (`post_merge by_corpus`). Mesh: `mesh_hits=0 mesh_corpora={}`, and roster plus plan took 0.13 s. Notes and memory recall have no stage of their own (`dossier:computed_for_turn` took 7 ms). The 9,987-note store does not show up in this turn's time. The two watched-folder corpora are searched like any other corpus.
- Atlas grounding. 21 corpora have a walkable store and 244 log `no store the walk can read`. Two spans of about 3.7 s each (08:04:45.23-48.96 and 49.12-52.87 in turn 3) have no info event, so the next reader cannot see what they are.
- Synthesis and gate. The cost follows from finding 20 chunks; the clean root found none.

Findings, filed here, none of them in this row's commits:
- finding: on the stream KQ path the throughput lane's `total_latency_ms` is synthesis plus gate (turn 1: 20,013 = 12,987 + 7,016) and leaves out routing and retrieval. The ship gate's 24,620 / 29,238 against 667-919 ms compared synth+gate over 20 chunks with synth over 0. The lane does not time the whole turn (sovereign-cli-bench quality_lane_cmd/throughput.rs:700, sovereign-core runtime/handlers/knowledge_query.rs:1981) (phase-d).
- finding: atlas grounding spends ~7.5 s per turn with no event at info. Principle 1. (cleanup)
- finding: the e2e arm says "no corpus attached", but on a node with installed corpora the turn searches all of them. The arm is a different turn on every node that holds corpora (throughput.toml `[e2e]`) (phase-d).

What would falsify this: a clean root seeded with the deployed node's 380 corpora that answers in about 0.7 s, or a deployed turn whose stage sum does not reach its wall time. In turn 3 the stages sum to 35.9 s against a 35.9 s wall.

</details>

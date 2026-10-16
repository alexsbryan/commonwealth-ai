<!-- ledger -->

**every-kind-finish-2 · 2026-10-15 · REVIEW-audit-ek-verify · director** — this commit
- Needed: every-kind-finish-1 said `ek-window-ref-labels` "changes no RESOLVE question"; that was wrong. On saltgrass-asked the newly Supported speaker labels change the atoms and so RESOLVE's candidate sets, and the row's `--asker recorded` resolve asked the local model 9 new RESOLVE questions (18 chat + 2 embed lines; store 293 -> 313, mtime moved). The worker asked: accept, or treat as a spend breach (truncate to 293).
- Chose: accept the 20 recorded lines; the audit row's saltgrass resolve becomes `--asker replay`, so re-running it can no longer reach a model, and the mtime rule reads "a prompt or the replays' inputs" changed. TESTALL and PREPUSH stay as the row has them.
- Because: no RESOLVE template changed in 60eeeb73c..HEAD (the only diffs under `resolve_records` are the `REASON_TOKENS` re-export and the new `reasoned_wave`, both used by waves only), so the new questions are the row's intended reading change reaching RESOLVE, not a prompt change. The spend was the row's own prescribed command, local, zero external tokens; truncating would not un-spend it and would leave the audit permanently §6. Keeping the lines is reversible (drop lines 294-313).

<!-- appendix -->

## every-kind-finish-2 · 2026-10-15 — saltgrass's 9 local RESOLVE answers stand; the audit's resolve replays

<details><summary>reasoning, evidence, package</summary>

Evidence, reproduced by the director:

- `git diff 60eeeb73c..HEAD -- .../atlas/resolve_records.rs .../resolve_records/select.rs`: the
  re-export line and `const REASON_TOKENS` -> `pub(crate)`; `wave.rs` adds `reasoned_wave` only.
  No `*prompt*` or `.md` template under ingest/ changed in the range.
- `tail -n 20 ~/.svrnmesh/enrichment/saltgrass-asked/answers.jsonl`: 9 resolve_reason + 9
  resolve_select (commonwealth/primary), 2 embed (qwen3-embedding-0.6b).
- `sovereign enrich resolve saltgrass-asked --asker replay` (director, after the worker's run):
  exit 0, 0 `no recorded answer`, `read statements as atoms: 8 state(s), 0 relation(s)`, store
  mtime 1792091658 / 4,336,213 bytes before and after, and the (type, attribute, outcome)
  reference counts identical to the worker's table (18 speaker decided, ledger hand/vessel
  decided).

Rejected: truncating answers.jsonl to 293 lines. The questions were already asked; removing the
answers makes every replay print `no recorded answer`, so the audit could never pass without
asking them again.

Falsified if: a later `--asker replay` resolve on saltgrass-asked at this HEAD prints
`no recorded answer` or moves the store's mtime, or a diff shows a RESOLVE prompt template changed
in 60eeeb73c..HEAD.

REVIEW-AFTER: the charter leaves model spend to the operator. This accepts spend already made by the
row's own command (9 local questions, zero external tokens) and spends nothing further; the operator
may drop lines 294-313 of the store if it should not stand.

</details>

**Correction to every-kind-finish-2 (2026-10-15, operator's seat).** "Local, zero external tokens" was wrong:
`commonwealth/primary` is the daemon's remote engine, `deepseek-flash` (`~/.svrnmesh/config.toml` `[engine]`,
since the 2026-10-13 ceiling test), so the 9 RESOLVE answers were DeepSeek Flash calls. The operator kept them,
labelled in `research/ontology-apps/saltgrass/resolve-labels.jsonl` (6901ea816, whose message also says "local").

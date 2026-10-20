<!-- ledger -->

**every-kind-finish-1 · 2026-10-15 · REVIEW-audit-ek-verify · director** — this commit
- Needed: the audit (findings e68365a17) found ek-window-refs' outcome unreached on saltgrass-asked, because the window model writes ref fields as its own particular labels (`p6`) and `window.rs` `reading` refuses them before projection; and TESTALL (14644/4) and PREPUSH (arch-gate) red on files outside the range. The package asked whether the label repair is its own row, and whether the audit closes against the range.
- Chose: (1) one repair row, `ek-window-ref-labels`, inserted before the audit, which now depends on it and re-runs the same saltgrass replay after it: a Ref value equal to a same-window particular's `ref` of type `of` (or is_a it) reads as that particular's name, in both `reading` callers, CONTRACT_VERSION bumped, no prompt change. (2) The audit row keeps TESTALL and PREPUSH and now says how to read a red: files with no commit in 60eeeb73c..HEAD are recorded foreign, a red on a file this queue touched is §6.
- Because: ek-window-refs' own text ("a window read's ref field resolves to the record its window read under that name") implies the fix; the label IS the window's name for the particular, and the charter decides fixing a row whose premise the tree contradicts. It changes no prompt and no RESOLVE question, so it replays at zero spend. Closing on the campaign's share with foreign reds recorded is the settled precedent (A10 rd-1, browser-dial-3, the-link-7); the bar is unchanged for anything this queue touched.

<!-- appendix -->

## every-kind-finish-1 · 2026-10-15 — speaker labels get a repair row; foreign gate reds are recorded, not §6

<details><summary>reasoning, evidence, package</summary>

Evidence, reproduced by the director:

- `~/.svrnmesh/enrichment/saltgrass-asked/runs/questions-full-007.json` (11:52): every `speaker`
  reading is unknown; the reasons are `"pN" is neither a character this document's fields name nor
  words it holds` (p2, p3, p4, p5, p6, p8, p10) and `the window read states no value`. Not one is
  Supported.
- `~/.svrnmesh/indexes/saltgrass-asked/atlas/derived_decisions.jsonl`: reference lines are only
  (account, place, nothing) 10, (notice, place, nothing) 16, (sighting, place, nothing) 1.
- `window.rs:418-457`: the Ref arm accepts a named Record/Mention candidate or words in the
  document, and nothing else. `window_prompt.md:16` asks for `"ref": <your label, such as p1>` and
  :8 has claims name particulars by `ref`. So the label never reaches `read_fields.rs` `record_read`
  (third rule, `SUBJECT_NAME_ATTRIBUTE`).
- Foreign reds: `git log --oneline 60eeeb73c..HEAD -- <file>` is empty for all nine named files
  (f26_egress_census.rs, chat_round_trip.rs, identity_and_health.rs, found_and_join.rs,
  engine_conformance/{run,server}.rs, desktop commands/reading.rs, llama_router/live.rs,
  model_slot.rs).

Rejected: reopening ek-window-refs. It is committed and its rule is right; the gap is upstream in
the window envelope, and a separate row is revertible on its own. Rejected: a prompt change telling
the model to write names (charter: the operator's, and it spends).

Falsified if: after `ek-window-ref-labels`, the saltgrass replay still shows zero
(type, `speaker`, decided) reference lines while some speaker particular is the subject of a
statement in the same document. That would mean the label is not the blocker, or the third rule
does not fire. Also falsified if the replay prints `no recorded answer` or moves an answer-store
mtime, which would mean the repair changed a prompt.

REVIEW-AFTER: the precedent closes audits on a campaign's own share, but no clause of this charter
names it. The arch-gate red (`model_slot.rs` 6146->6209, band +5 files) blocks pre-push for the
whole branch until its owner splits it; it belongs to the lane that landed c4a0b8f61..96921a3a8.

</details>

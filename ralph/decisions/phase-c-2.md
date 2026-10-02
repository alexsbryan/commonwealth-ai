<!-- ledger -->

**phase-c-2 · 2026-10-02 · seat, filing phase-b's remaining untracked findings to phase-c · seat** — this commit
- Needed: target/ralph/phase-b/preflight-forks.md, an untracked file, still held findings no queue named. Re-read at 89b900c9d, fifteen held, two were fixed already (the `dir.pop()` climbs in mesh_principal_gate.rs and mesh_proof_header_gate.rs are gone; sovereign-lint.sh no longer names `mesh-sim`), and two are operator edits to AGENTS.md (clone-gate absent from the "Rules with a ratchet" list; the verb→binary table), which no row may make.
- Chose: three rows below the cleanup cut line, outside the frozen scope: pc-mesh-dissolve-residue (seven files naming the deleted sovereign-mesh, or a moved test, as live), pc-retired-verbs-named (`svrn milestone`, `svrn drift <feature-id>`, `svrn plan`, `svrn project found` answer outside `deprecation::RETIRED`; two exit 0), pc-test-port-toctou (two release-then-bind e2e flakes; 24 private `fn free_port`). size-gate's red keys stay the merge's closing re-pin at origin/main, not a row.
- Because: a finding in an untracked file is filed nowhere (phase-b-97, -104, -109, -110). None changes what a user can do except pc-retired-verbs-named, whose harm is a misleading error on a verb that no longer exists; it is a one-id scope move if the operator wants it inside phase-c.

<!-- appendix -->

## phase-c-2 · 2026-10-02 — phase-b's remaining untracked findings are three rows below phase-c's cut line

<details><summary>reasoning, evidence, package</summary>

Each item was re-read in the tree before filing. `svrn milestone foo 1` → "requires --project", exit 2; `svrn drift foo` → "requires a subcommand", exit 2; `svrn plan` and `svrn project found` → retirement note, exit 0; `svrn mobile` → "no mobile host ships", exit 1, against cli-contract.toml:3629's `disposition = "promote"`. `grep -c '^\[\[family\]\]' quality/twin-plants.toml` = 7 against SYSTEM_OVERVIEW.md:263's "19 families". `grep -rln 'fn free_port'` over the Rust trees = 24 files.

Not filed: client_tokens_e2e's libtest-isolation case has no red run since the closing sweep (0 of 7); it rides pc-test-port-toctou as "reproduce first". corpus_watch_http_e2e was fixed in 3c08179cc. code_server_via_mcp_client.rs finds its binary beside the test with no `*_BIN` knob, but LIFT(svrn) is green at 327a8097b, so it is not a defect today.

</details>

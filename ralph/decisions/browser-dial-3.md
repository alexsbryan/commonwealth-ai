<!-- ledger -->

**browser-dial-3 · 2026-09-22 · REVIEW-audit-browser-dial · director** — this commit
- Needed: The audit body was done and committed (`418639ea7`) but the row stayed `[~]` on its two named checks, both red on causes the campaign's diff does not touch. TESTALL exit=100, 13710 pass / 2 fail: `every_journey_cites_a_doc_that_exists` — mesh-offers-catalogue cites `docs/internal/RING_APPLICATIONS.md`, gitignored per-host (`.gitignore:67`), absent here, cited at `sovereign/docs/cli-contract.toml:3627` since `a3bd715f5`, an ancestor of BOTH `origin/main` and BASE — and `local_only_boot::a_local_only_daemon_spawns_no_network_service`, the 10 s boot bound under a 13712-test parallel run (`testfn sovereign-mesh` alone exit=0). PREPUSH exit=1: arch-gate [hard], the approach band 203149 → 203154 (+5), in full the concurrent foreign `5825a2fa3`'s `ring_cmd/mod.rs` 1082 → 1087. The predicate's literal reading — tree-wide `git diff --stat <BASE>..HEAD` over sovereign/crates and commonwealth/crates empty — is FALSE: +678, all of it foreign. The package posed the disposition as operator forks.
- Chose: Close the audit row on the campaign's OWN share, which is zero. (1) The predicate is read per its two bars' `floor_basis` clause (d), which already scope it "for this row", and per O §Predictions ("sovereign/crates: +0 … a row that needs a product edit has found the order's exit condition"); the tree-wide literal reading is recorded FALSIFIED by the foreign lane, not erased (ARCH 6). (2) The two TESTALL reds are recorded foreign, as this queue's precedent (the-link-7, A51 rr-2, A59 ring-guest) closed the identical red; the restore/rename of the per-host doc stays the operator's. (3) arch-gate [hard] is LEFT RED and packaged — its only fix is a product edit (`ring_cmd/mod.rs` trimmed/split 5 lines), which this campaign's one absolute forbids and which the foreign lane that grew it owns; a working-tree `--update-baseline` is forbidden by PROMPT §7, and a re-pin at origin/main cannot absorb an unpushed commit (`5825a2fa3` is not on origin/main). (4) The dial WORKED — bd-1 returned `HTTP/1.1 200 OK` carrying `ring-658e43cce7830b48` (browser-dial-2) — so the order's DoD (O Demo: the MEASUREMENT closes the order) is met, and the demo-leg follow-on order stays the operator's (campaign.md Stop conditions). Every row is `[x]`.
- Because: A measurement campaign's contract is a zero product diff and a measured dial; both hold on the campaign's own share (its four commits are ledger + queue mark only; its own `.rs` change is zero). Correcting a row whose premise the tree contradicts is the operator's standing direction (`ralph/PROMPT.md`, 2026-09-17), and closing an audit with a foreign red recorded is the-link-7's settled precedent. The one thing the charter does not clearly cover — a foreign HARD-ratchet regression inside the range whose only fix is a product edit — is why this decision carries REVIEW-AFTER. REVIEW-AFTER: the approach band is left +5 over baseline on foreign, unpushed product code; pre-push stays BLOCKED for the whole branch until the operator or the `5825a2fa3` lane trims/splits `ring_cmd/mod.rs` back under 1082 (or banks a real cut with `arch-gate --tighten`).

<!-- appendix -->

## browser-dial-3 · 2026-09-22 — close on the campaign's own share — both reds foreign, arch-gate left red for the operator

<details><summary>reasoning, evidence, package</summary>

**The fork.** The package offered four forks: (1) accept the zero-own-share
reading or treat the foreign tree-wide diff as a predicate failure; (2) split
`ring_cmd/mod.rs`, re-pin, or accept the arch-gate red; (3) the two TESTALL
reds; (4) close or hold. The charter decides (1) and (3) and the row
disposition (4), and leaves product edits and the WORKED-dial follow-on to the
operator; the arch-gate fix (2) is the one case the charter does not cleanly
cover, because it is a product edit and the charter's one absolute — the
product diff is empty — forbids the delegate from making it. This decision
records the resolution and flags that residual.

**Why the predicate is read per-row, not tree-wide.** The predicate statement
(`quality/campaigns/browser-dial.toml:21`, `.sovereign/features/browser-dial/
campaign.md:16`) is prose; the scored object is each bar's `floor_basis`
clause (d), which says "git diff over sovereign/crates and commonwealth/crates
**for this row** is empty". Both bars read 1.0 on that floor. O §Predictions
scopes the same claim ("sovereign/crates: +0") to this campaign and names a
product edit as the order's exit condition. The campaign's own four commits
change no `.rs` line — `git show --stat` of `a1cb78f60`, `5f58c18d4`,
`ff6d8e561`, `51b64f68a`, `418639ea7` is ledger/queue/REVIEW_FINDINGS only.
The tree-wide literal reading IS false; that is the audit's loudest finding and
it is recorded, not substituted away (ARCH 6).

**Why the TESTALL reds are foreign.** `a3bd715f5` (the citation commit) is an
ancestor of `origin/main` (verified: `git merge-base --is-ancestor a3bd715f5
origin/main` → 0) and of BASE; `sovereign/docs/cli-contract.toml` is untouched
by the campaign range; `docs/internal/` is gitignored wholesale
(`.gitignore:67`) so the red is per-host by construction. The
`local_only_boot` failure is load: the same test passes alone
(`./scripts/ralph-check.sh testfn sovereign-mesh
a_local_only_daemon_spawns_no_network_service` → exit=0, 1 pass). Both are the
identical reds the-link-7, A51 (rr-2) and A59 (ring-guest) recorded.

**Why the arch-gate red cannot be cleared here.** Reproduced this session:
`./scripts/ralph-check.sh arch` → exit=1, "approach band GREW: lines 203149 ->
203154 (+5)". Sizes: `ring_cmd/mod.rs` 1087 (was 1082 at BASE), the only one of
the four foreign files in the 800–1200 band (`serve.rs` 427, `mesh_media.rs`
509, `origin.rs` 240). The three charter-legal levers all fail: trimming
`ring_cmd/mod.rs` is a product edit; `--update-baseline` is forbidden by PROMPT
§7; a re-pin at `origin/main` already describes 203149, and `5825a2fa3` is not
on `origin/main` (`git merge-base --is-ancestor 5825a2fa3 origin/main` → NO).
So the band is left red on foreign code, and the branch's pre-push stays
blocked until that lane fixes it.

**What would falsify this decision.** A browser-dial commit that changes a
`.rs` line (there is none: the campaign's own diff over product paths is
empty); the arch-gate +5 traceable to a campaign commit (it is `5825a2fa3`'s
`ring_cmd/mod.rs`: `Some("serve") => run_serve(...)`, its help line, `mod
serve;`); the per-host doc resolving on a fresh clone (it cannot — the
directory is gitignored); the `local_only_boot` test failing alone (it passes).

**Evidence reproduced this session:** `./scripts/ralph-check.sh arch` exit=1;
`testfn sovereign-cli every_journey_cites_a_doc_that_exists` exit=100 / 1 fail,
"mesh-offers-catalogue cites `docs/internal/RING_APPLICATIONS.md`, which does
not exist"; `testfn sovereign-mesh
a_local_only_daemon_spawns_no_network_service` exit=0; `git diff --numstat
ac44e6fec..HEAD -- sovereign/crates commonwealth/crates` = +678 in exactly the
four `5825a2fa3` files; `git merge-base --is-ancestor 5825a2fa3 origin/main` →
NO; `a3bd715f5` an ancestor of both `origin/main` and BASE.

**Loudest finding for the operator (also in `ralph/REVIEW_FINDINGS.md`
§browser-dial):** the concurrent foreign lane `5825a2fa3` + `7fef0a9dc`
(operator direction 2026-09-22, "I want to go 100% cli") is inside
`BASE..HEAD` and is the sole cause of both the predicate's literal falsehood
and the pre-push block. This campaign neither owns nor may fix it.

</details>

<!-- ledger -->

**phase-b-88 · 2026-10-01 · pb-distribution-onprem-compose · seat, ruling where ingest's hosting composition lives once rule 3c reads `#[path]`** — this commit
- Needed: B's on-prem rows cannot land on cut. sovereign-onprem/src/main.rs:17 mounts `#[path = "../../sovereign-stock/src/ingest.rs"]`, and cut's rule 3c (4dd8f5a7b, `EscapeKind::PathMount`) reports a mount out of the crate root, so BOUNDARY 1 -> 2 on landing. `hosted()` names svrn's `process::HostedIngest`/`IngestCalls`/`IngestMount` and ingest's face items together, so only a crate outside every package can hold it, and `arch_layers::distributions::validate` refuses a crate claimed by two `[[distribution]]` rows ("a composition root has one row"). Checked at f38d9f24c.
- Chose: a library crate `sovereign-hosted-ingest`, in no package and no leaf, holding `hosted()` moved verbatim from sovereign-stock/src/ingest.rs. BOTH distribution rows list it in `crates`, and a crate several rows claim answers to EVERY row that claims it: its direct edges are judged per claiming row (`evaluate_distributions` iterates the claiming rows instead of taking the first), its `src/` face-item scan runs once per row, and its code lines count against each row's fixed cap. The two-row refusal in `validate` goes; the package-member and leaf refusals stay. sovereign-stock (both bins) and sovereign-onprem call `sovereign_hosted_ingest::hosted(..)`; the `#[path]` mount is gone.
- Because:
  - Principle 8. One composition of ingest for svrn, never a copy. A copy in sovereign-onprem would compile-track the struct fields but not the choices (which extractor, which client), which is exactly the drift one decider prevents.
  - Principle 10. On-prem's withholding becomes structural for the shared code too: the crate is judged against on-prem's faces, so naming `sovereign_recipe_author` or `sovereign_code` there is a violation under on-prem's name. Today that property is a sentence in ingest.rs's doc comment.
  - Principle 12. Composing programs is a distribution's job; the crate belongs to no program. The rejected homes each put the composition in a side that does not own it: svrn's daemon would link corpus-engine and the catalog (svrn's lift breaks), and an ingest face crate would name sovereign-daemon (an [ingest] -> [svrn] edge).
  - Principle 11. It reuses the distribution rule and its caps; no new row kind, no `uses` key, no exception. The intersection is the strictest semantics a shared crate could have.
  - Caps hold without a re-pin: on-prem 76 + 61 = 137 of 200, stock 137 + 6 + 61 = 204 of 300 (non-blank, non-comment lines, approximate).
- REVIEW-AFTER: the landing on cut. Falsified if a PLANT that names `sovereign_recipe_author::port::compose` in sovereign-hosted-ingest's `src/`, or adds a sovereign-recipe-author edge to its manifest, does not go red under `[onprem]`; if BOUNDARY on cut after the landing is not the pre-landing count; or if the stock install's ingest journey changes.

<!-- appendix -->

## phase-b-88 · 2026-10-01 — ingest's hosting composition is a crate both distribution rows claim

<details><summary>reasoning, evidence</summary>

The gate's own fix text for a `PathMount` is "put the shared code in a crate both depend on". For distributions that crate could not exist: `validate` refused it by name. The refusal was written when every distribution was one binary crate, and it protected against one crate being judged under two inconsistent rule sets. Judging it under both, and failing it under either, keeps that protection and admits the shared crate.

Rejected: an ingest-package face item (the seat's earlier lean in frame c06eb471) — `hosted()` must name `sovereign_daemon::process::HostedIngest`, and an ingest crate may not reach svrn. Merging on-prem into the stock row loses on-prem's narrower face list. A `uses = [..]` key on the row is a second mechanism for the same reachability question.

</details>

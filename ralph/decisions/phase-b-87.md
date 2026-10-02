<!-- ledger -->

**phase-b-87 · 2026-09-30 · pb-distribution-onprem-compose · director** — this commit
- Needed: the worker stopped at census with four false premises in the row's PROOF, each reproduced at b1fe73dcd:
  - Tool ids are routing data in crates every svrn daemon links (sovereign-contracts routing.rs:300-357, intent_policy.rs:365-482, tool_bundle.rs:133-151), and WebFetchTool lives in sovereign-tools, which sovereign-daemon links (Cargo.toml:51). A `strings` check for `web_fetch` cannot pass on a correct build; package.sh:118-129 already recorded this for the old server.
  - `/mcp` is svrn's own route, merged whenever `McpSurface::Mounted` (daemon.rs:1613-1621); `code: None` only removes code's tools from it.
  - `/v1/solve/jobs` with `code: None` answers the named 503 of `solve_absent_router` (hosted_code.rs:118-122, FIVE_PROGRAMS §4 rule 3), not 404.
  - `IngestCalls.recipe_authoring` (hosted_ingest.rs:107) is required, so a root composing ingest must link sovereign-recipe-author and its ProbeUrlTool.
- Chose: rewrite the row, keeping its outcome.
  - Tool absence is proven by the running registry, never by `strings`. `strings` checks only literals that sovereign-code or sovereign-recipe-author alone carry (`/v1/solve/jobs/{id}/events`, solve_http.rs:767), on onprem (absent) and stock (present, watched red).
  - One posture value through `process::run` carries web reach, the wikipedia bundle and the `/mcp` ROUTE. Withheld `/mcp` answers a named 503. The `McpMount` stays, because its notes store backs the notes routes (daemon.rs:544-554).
  - `/v1/solve/jobs` keeps its named 503.
  - `IngestCalls.recipe_authoring` becomes an `Option`, with five sites in the census. Only sovereign-stock links sovereign-recipe-author, so the onprem closure holds no ProbeUrlTool.
  - The ARCH_LAYERS row holds direct edges only. The closure keeps commonwealth-rail-core and sovereign-tools through sovereign-daemon, and the row now says so.
  - pb-distribution-onprem-kit's acceptance check 0 reads "never 2xx" (404 or a named 503), and its package.sh gate uses the same literals. FIVE_PROGRAMS §2c gains the posture paragraph.
- Because:
  - A check that cannot pass on a correct build is not a gate (principle 5).
  - Absence is named, never a bare 404 (principle 6, §4 rule 3).
  - The withholding is structural: the registration and the route are never built (principle 10). The existing seams are reused: `Option` parts, `Withheld`, `solve_absent_router` (principle 11).
- Falsified if: the onprem registry census lists any withheld tool; `/mcp` on onprem answers 2xx; the solve-events literal is found in the onprem binary; or making recipe authoring optional changes what stock or cli-llm-stock does. This entry also amends phase-b-86's falsifier: "contains a withheld tool's strings" now means the code-only and recipe-author-only literals, not the tool ids.

<!-- appendix -->

## phase-b-87 · 2026-09-30 — on-prem compose: prove absence by registry and route, not by tool-id strings

<details><summary>reasoning, evidence, package</summary>

Reproduced commands (the main checkout at b1fe73dcd):

- `git grep -n '"web_fetch"\|"wikipedia_fetch"\|"probe_url"' -- sovereign/crates/sovereign-contracts/src` gave 18 non-test hits across intent_policy.rs, tool_bundle.rs, types/routing.rs and skills_data/recipe-author.toml.
- `grep -ln sovereign-recipe-author sovereign/crates/*/Cargo.toml corpus-engine/Cargo.toml` matched only sovereign-stock/Cargo.toml.
- sovereign-daemon/Cargo.toml:28 is commonwealth-rail-core and :51 is sovereign-tools. Its `ocr` feature (:140) forwards sovereign-tools/paddle-ocr.
- `McpSurface` (daemon_services.rs:121-138) has `Mounted` and `Unavailable`. `Unavailable` means "could not build", which is a different fact from a distribution's choice. That is why the posture gates the route merge and leaves the mount alone: `notes_store()` reads through the mount.
- I did not re-run the worker's `strings` counts (43/45/57 on debug stock). The source census above is enough to decide the fork.

Options considered for `/mcp`: (A) a posture on the route, chosen; (B) leave it mounted and rely on nginx not proxying it, rejected because it is remembered, not structural (principle 10). For `/v1/solve/jobs`: an on-prem-only 404 was rejected, because it would be a second answer to the absent-code question, against §4 rule 3.

</details>

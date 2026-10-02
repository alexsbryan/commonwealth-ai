<!-- ledger -->

**phase-c-8 · 2026-10-01 · pc-split-deploy-honesty · worker** — this commit
- Needed: the row's third bullet (was pc-mesh-status-serve-down) asks that `/v1/mesh/status` tell "serve down" from "serve slow"; the census found no route that answers about serve's health at all.
- Chose: build the first two bullets (c09c05eb8 reload refuses by name, a0a241d45 svrn follows serve's self-report), and correct the third bullet's premise in the row instead of inventing a new status field on another route; a serve-reach field on svrn's `/status` is left as a finding.
- Because: `/v1/mesh/status` is cw-rails' since pb-mesh-exit-transport (sovereign-daemon mesh_http.rs:34 `MOVED_TO_RAILS`, 410 with a pointer; commonwealth-rails api.rs:194 `status` reads the roster only), and the one reader that kept the two absences apart (`sovereign_turn_client::serve_self::read_engine_state` / `EngineStateRead`: `Unreachable` vs `DidNotAnswerInTime`) has no production caller. No answer collapses down and slow, so the defect as filed has no site; putting serve's reach on `/status` would be a behaviour delta the row does not state (§7 "a delta the row does not state is §6").

<!-- appendix -->

## phase-c-8 · 2026-10-01 — pc-mesh-status-serve-down has no site after the flip

<details><summary>reasoning, evidence, package</summary>

The review that filed it (ralph/PHASE_B_SHIP_GATE.md "Not blocking") predates the flip that moved
`/v1/mesh/status` to cw-rails. Evidence, at a0a241d45:

- `git grep -n '"/v1/mesh/status"' -- '*.rs'`: served only by commonwealth-rails api.rs:72; svrn's
  mesh_http.rs:34 lists it in `MOVED_TO_RAILS`.
- `git grep -n "read_engine_state\|EngineStateRead" -- '*.rs'`: defined and tested in
  sovereign-turn-client/src/serve_self.rs only; the f26 egress census comments name it.
- `svrn mesh status` (sovereign-cli-mesh mesh_cmd.rs `cmd_status`) reads cw-rails and prints no serve line.
- svrn's `/status` `serving` (routes_status.rs:292) is the boot-decided path name (`serve` /
  `serve (this process)`), not a health claim.

What would falsify this: a route or verb found that answers serve's health and collapses an
unreachable serve with a slow one; or the operator ruling that svrn's `/status` should carry serve's
reach, which is a new row (the follow loop of a0a241d45 already reads serve every 10 s and could feed it).

</details>

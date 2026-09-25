# phase-b — the ralph queue (STAGED, not started)

Staged by the operator's direction 2026-09-24 (ralph/decisions/five-programs-39.md):
Phase B is the NEXT campaign, started when five-programs completes. Nothing
runs this queue until the operator launches it. five-programs ends at its
`HUMAN-phase-b` row, after its `REVIEW-handoff-phase-b` has written the edge
list this campaign inherits into the appendix below.

At launch, write this queue's `CHARTER.md`, `PROMPT.addendum.md` (render
`PROMPT.md` with `python3 scripts/ralph.py prompt --queue phase-b`) and
`queue.toml`, using five-programs' as the base, then start it the way
five-programs' queue.toml header shows, with `--queue phase-b`.

What Phase B is: §11's front-loading step 3 in docs/FIVE_PROGRAMS.md, "the
serving-cluster dial". It widens to every red edge whose only closing arm was an
`[[exception]]` or a new serving host (operator, five-programs-38: build the
host, not the exception). Its finish condition is five-programs' original
condition 1: `cd corpus-engine && cargo xtask boundary-gate` exits 0 with no
exception added by Phase B, and with the fp-9/fp-10 exceptions retired.

Operator direction 2026-09-25 (ralph/decisions/five-programs-54.md): a PURE five-program outcome with the
MINIMAL lift — no standing boundary exception at the end, reached by deleting or moving before building,
reusing before minting, and the smallest host that serves each verb. The work-atlas question (charter §5
lists the work atlas as deleted; sessions call `work_in_flight`) owns daemon → sovereign-work-atlas and
fp-84's dev edge sovereign-mesh → sovereign-work-atlas.

- [ ] REVIEW-plan-fp-phase-b — depends [] — PLAN Phase B (operator, 2026-09-24, five-programs-38 "Start Phase B now", staged as the next campaign by five-programs-39). Every edge whose only closing arm was an [[exception]] or a new serving host closes by building the host, not by exception. No code. (1) CENSUS the red edges: the handoff list in this file's appendix (five-programs' REVIEW-handoff-phase-b wrote it), reproduced against the gate at this row's commit. It covers the edges of five-programs' inherited rows (fp-11, fp-12's seven pairs, fp-25, fp-34, fp-43, REVIEW-mint-fp-core-dial), its NEEDS-OPERATOR lines, and the edges the fp-9/fp-10 [[exception]] rows grant (granted "until Phase B", so Phase B retires them). (2) For each edge name the program §2 says OWNS the verb the edge carries and the process that will serve it; group edges by serving host, ONE campaign per host. Expected, for the census to falsify: the code program's MCP server with the reindexer and ScipGraphHandle; an ingest serving surface; the serving-cluster dial proper (fp-10's exception retires); a cmnwlth host for the sovereign-* crates cw-rails may not name (grants, meshapp, meshapp-registry — re-home or serve, decided against quality/ARCH_LAYERS.toml's [[forbid]] rows); the pods worker exec split (TSV:23); setup's exec phase (HUMAN-fp25 (a)). (3) MCP SURFACES follow one rule (operator, five-programs-39): the CLIENT composes the surface — each program serves its own MCP wire (§2) and the harness config lists every server; each program OWNS its own lifecycle by connect-or-spawn (for code: `svrn code mcp` over stdio dials the one code server, or starts it detached under sovereign-contracts' run_lock and idles out with no clients); there is NO daemon proxy — for a moved tool the daemon answers a named pointer ("moved to svrn code; `svrn project init` updates your config"), never unknown-tool (rules 3, 6). The first split is mcp_router (sovereign-daemon/src/mcp_router.rs:166-183): a generic transport over ToolRegistry to a leaf both programs mount, and the code program's call observer (ToolPatternMatcher) on the code mount only — delta: tool-call patterns that span a knowledge tool and a code tool stop being observed. The scaffold (sovereign-cli/src/project_init/scaffold.rs:509-536) writes both servers; this repo's .mcp.json and .opencode/opencode.json change with it. (4) NODE IDENTITY (operator, five-programs-38, timing corrected by -39): ONE node key owned by cw-rails; the daemon's ~/.svrnmesh/node_key retires in the campaign that retires the daemon's own mesh endpoint (fp-9/fp-10's exceptions), fp-74's attestation is then signed by rails at the daemon's request, and the existing-install key migration ships in the same commit. (5) MINT ONE REVIEW-mint row per campaign, each stating its lift (rows, files, new capability) and ordered by edges closed per unit of lift where dependencies allow (operator, five-programs-54), ordered host before client, each carrying its edge list, the existing surface it reuses (principle 11 — cw-rails' doors, /v1/admin/hardware, rails_client, run_lock), and every behaviour delta it causes with its reader. A delta that five-programs-38/-39 and §12 do not already name is a NEEDS_HUMAN line, never a silent change (principle 6); first-run and standalone behaviour are preserved or the campaign halts. Re-mint the inherited rows into their campaigns and rewrite each appendix owner cell. — read: docs/FIVE_PROGRAMS.md §2, §4, §11 front-loading, §12; this file's appendix; ralph/next/five-programs/STATE.md (the inherited rows and their director notes); ralph/decisions/five-programs-{28,30,31,32,38,39}.md; quality/ARCH_LAYERS.toml — check: every red edge on the gate at this row's commit has exactly one owner cell (the gate count and the owner histogram in the commit body); campaigns ≤ 8, past it NEEDS_HUMAN with the count; no code

## Appendix — the handoff list

Written by five-programs' `REVIEW-handoff-phase-b` on 2026-09-25, at the gate on `cut` after 83fbdc620:
**boundary-gate 51 violation(s)**, 51 dep edges (49 normal, 2 dev) and 0 structural. The build.rs line closed with fp-105.
Every edge has exactly one appendix line in ralph/next/five-programs/STATE.md. The per-edge census, with sites
and counts, lives on that line, and the class is repeated here.

Owner histogram: 16 edges to the 8 inherited rows (fp-12 7, fp-11 2, fp-43 2, fp-25 1, fp-34 1, fp-47 1,
REVIEW-mint-fp-core-dial 1, REVIEW-mint-fp-mesh-dial 1). 35 edges to `phase-b`: 32 were NEEDS-OPERATOR lines,
and 3 are the ingest-dial class that HUMAN-fp7 (a) folded fp-7 and fp-29 into. Unowned: 0. There are also 5
excepted edges that are not red and that Phase B retires.

### Inherited rows (re-mint into their campaigns)

- [svrn] sovereign-cli-daemon → sovereign-inference — fp-25 (setup's exec phase, HUMAN-fp25 (a))
- [code] sovereign-cli-dev → sovereign-daemon — fp-11
- [svrn] sovereign-daemon → sovereign-code — fp-11 (the code MCP host; five-programs-39's MCP rule)
- [code] sovereign-cli-dev → corpus-engine — fp-34 (read mounts after fp-11's /mcp root; `code finalize`/`code watch` residue)
- [svrn] sovereign-cli-llm → sovereign-authoring-harness — fp-43 (the drive home, now an ingest dial)
- [svrn] sovereign-daemon → sovereign-authoring-harness — fp-43
- [svrn] sovereign-daemon → commonwealth-media — fp-47 (app registry's one owner)
- [svrn] sovereign-daemon → sovereign-meshapp-registry / sovereign-meshapp / sovereign-grants / code-next-edit / sovereign-tdd / sovereign-pods / sovereign-gliner — fp-12 (seven pairs)
- [svrn] sovereign-daemon → commonwealth-core — REVIEW-mint-fp-core-dial (fp-6 roster landed; residue)
- [svrn] sovereign-daemon → sovereign-mesh — REVIEW-mint-fp-mesh-dial (fp-6..8 verbs landed; residue)

### phase-b (open classes: each needs an operator decision or a Phase B host)

- ingest-dial class (corpus-mcp membership; HUMAN-fp7 (a) keeps canonical pull and the executor daemon-side):
  [svrn] corpus-mcp → corpus-engine, [svrn] corpus-mcp → sovereign-enrichment-build,
  [svrn] sovereign-daemon → corpus-engine (183 residue leaves), [cmnwlth] sovereign-mesh → corpus-engine (6),
  [svrn] sovereign-tools → sovereign-recipe-author (fp-29: RecipeProjectStore, the shim is load-bearing),
  [svrn] sovereign-tools → corpus-engine (170 residue refs that EXECUTE ingest as MCP tools),
  [svrn] sovereign-runtime-recipe → corpus-engine
- cli-llm split (five-programs-23; shared CLI-helper home, bench turn drive): [svrn] sovereign-cli-llm →
  sovereign-enrichment-catalog, → sovereign-enrichment-build (normal and dev), → sovereign-gliner,
  → sovereign-pipeline, → sovereign-eval, → corpus-engine (397 residue leaves), → sovereign-inference (3 local GGUF loads)
- node identity (five-programs-38/-39, lands with fp-9/fp-10's retirement): [svrn] sovereign-cli-llm → sovereign-mesh
- provisioning: [svrn] sovereign-cli-llm → sovereign-pods
- notes factory: [svrn] sovereign-cli → corpus-engine-notes, [svrn] sovereign-cli-llm → corpus-engine-notes,
  [svrn] sovereign-daemon → corpus-engine-notes, [svrn] sovereign-tools → corpus-engine-notes
- code_index's home (fp-5's refusal stands, REVIEW-mint-fp-cli-shared-leaf 59c38d4b8): [svrn] sovereign-cli-shared →
  corpus-engine, [code] sovereign-cli-dev → sovereign-cli-shared
- wire boundary (§12 recommendation untaken; fp-45's `process` executor seam): [svrn] sovereign-cli → commonwealth-work,
  [svrn] sovereign-daemon → commonwealth-work
- D6 keep, daemon-route closing condition untaken: [svrn] sovereign-cli → corpus-engine
- transport leaf question: [svrn] sovereign-daemon → commonwealth-transport
- work-atlas question (five-programs-54): [svrn] sovereign-daemon → sovereign-work-atlas, [cmnwlth] sovereign-mesh →
  sovereign-work-atlas (dev, fp-84's)
- watcher runtime owner (TSV:12): [svrn] sovereign-daemon → corpus-engine-watchers
- [svrn] sovereign-runtime-recipe → sovereign-gliner — the runtime lane's entity-extractor probe (load_gliner)
- [svrn] sovereign-tools → sovereign-enrichment-catalog — the watched-folder config writer's owner
- [code] sovereign-cli-dev → sovereign-tools — SpecWatcher + the MCP surface list
- [code] sovereign-cli-dev → sovereign-enrichment-build — scoring types (ChatPrompt)
- [cmnwlth] sovereign-grants → corpus-engine — dial or drop

### Excepted edges Phase B retires (not red; quality/ARCH_LAYERS.toml [[exception]] rows with `package = "svrn"`)

- sovereign-daemon → commonwealth-discovery — fp-9
- sovereign-daemon → sovereign-inference, sovereign-daemon → sovereign-compute — fp-10
- sovereign-daemon → sovereign-serving-host — fp-68
- sovereign-runtime-recipe → sovereign-inference — fp-69

### Non-edge item

- corpus-engine's default recipe/asset source (five-programs-52): `corpus-engine/src/recipe_source/bundled.rs` is the
  one corpus-engine module that names the `sovereign-recipes` data crate (`corpus_engine_recipes`), behind
  `default_source()` / `default_assets()`. It lifts out to svrn's composition roots once svrn dials ingest. Owner:
  the ingest serving campaign.

# THE GIT INDEX IS SHARED STATE ACROSS SESSIONS, AND git add <your paths> IS NOT ENOUGH. Three occurrences on 2026-08-20 in this one…

THE GIT INDEX IS SHARED STATE ACROSS SESSIONS, AND `git add <your paths>` IS NOT ENOUGH. Three occurrences on 2026-08-20 in this one checkout; the third revised the rule.

WHAT HAPPENS: several sessions (seat + workers + peer workstreams) share ONE working tree AND ONE index. Nothing in git scopes a stage to a session.
- `git add -A` / `git add .` / `git commit -a` take everything uncommitted. Occurrence 1: `ba3ffcbd` took the nc-2c-A worker's code. Occurrence 2: `ac8f65a0`, a census-doc commit, also carried `scripts/nc-thesis.py` (+125) and 28 lines of campaign toml written by the seat minutes earlier.
- **AND SO DOES `git add <your paths>` FOLLOWED BY A BARE `git commit`** — because the commit takes THE WHOLE INDEX, including entries a peer staged. Occurrence 3 (nc-10's worker): a peer's staged file MOVE was swept into `5b59b5ce` despite only their own paths being added.

THE RULE:
1. **Commit with the PATHSPEC form: `git commit -- <paths>`** (or `--pathspec-from-file`). It commits only those paths and ignores the rest of the index. This is the form that recovered occurrence 3 (`reset --soft`, then re-commit as `84608e74`).
2. **`git show --stat HEAD` after EVERY commit.** If the file count or insertion count does not match what you wrote, something arrived or something left. The commit succeeds either way and says nothing. This is the only cheap detector.
3. For a long rung with many commits alongside an active peer, nc-10's worker went further and it worked: build blobs as HEAD+your-hunk, stage with `git hash-object -w` + `git update-index --cacheinfo`, park and restore the peer's index entries around each commit, verify each with `git show --raw --no-renames`. Peer index confirmed byte-identical afterward. Reserve this for when the pathspec form is not enough.
4. If your message overclaims because content went elsewhere, amend YOUR OWN commit and cross-reference the sha where it actually landed. NEVER rewrite the peer's commit — it may already have work on top, and rewriting breaks their clone.
5. `cargo fmt -p <crate>`, never `--all`; check peer files in those crates were not reformatted.

RELATED SHARED-STATE HAZARDS IN THE SAME CHECKOUT, all confirmed the same day:
- **The SCIP index is shared and a `git worktree` does NOT isolate it.** `scripts/nc-boundary.py` reads `~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db`, so its numbers describe `last_indexed_head`, move when the daemon re-indexes a PEER's commits, and lag HEAD (measured six commits behind). Fixed in `0f6bb56d` so the row stamps the indexed commit; a DELTA across two runs is still not automatically yours. Cross-check with `cargo xtask layer-gate`, which reads manifests from disk.
- **`target/` is shared.** nc-13's worker hit exit 5/101 on three corrupt 1.7KB non-executable test binaries left by a concurrent build. Not a code failure. Clear stale test binaries before a definition-of-done sweep rather than reading it as a regression.
- A targeted test can fail on a PEER's in-flight work. nc-10's `sovereign-core` run failed 1 of 1338 on `f26_egress_boundary_census`, which names `code_index` paths the kill-chain peer was mid-move on. Attribute before you fix.

DO NOT "fix" any of this with a lock file or by serializing commits. The shared checkout and the fan-out are deliberate. Detection plus honest cross-referencing is the cheap correct response.

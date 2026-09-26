<!-- ledger -->

**phase-b-4 · 2026-09-26 · invite rotated, lift hermetic after membership, test-load row · operator** — this commit
- Needed: `pb-lift-instrument` needs a live invite, and the invite had expired on 2026-09-23. Three load-sensitive flakes had no owner. The seat set out pros and cons, and the operator took both recommendations.
- Chose:
  - (1) The mesh join key was rotated through the live daemon, then measured by hand. The join WORKS at HEAD: admitted to Meshsonics, and the roster converged. The media step's 0 was the test's defect, not the lift's.
  - (2) pb-lift-instrument's run step reaches only ONLINE media offerers, exits 3 when none is online, quotes the route's refusal, and retires the member it joined as.
  - (3) After pb-membership, the cmnwlth lift founds its own two-node mesh with a fixture media origin, so repeated runs stay off the operator's mesh.
  - (4) pb-membership also covers a two-key node: an install that ran HEAD holds a solo cw-rails key beside the daemon's, and the daemon's key wins.
  - (5) A new row, pb-test-load, follows the census. It measures each flake over at least 5 runs, serializes daemon boots with a nextest test group, fixes the join-child port race, and raises no timeout.
- Because:
  - Principle 5: world state (every offerer offline) is could-not-judge, not failed. A test that discards the route's refusal cannot tell the two apart.
  - Principle 7: flakes are measured before they are fixed, and no judge is loosened in one direction.
  - Principle 8: identity from essence; peers' rosters hold the daemon's key.
  - Principle 10: tests stay off the host's real mesh, the same lesson as the 9747 leak.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-4 · 2026-09-26 — rotate the invite now; mesh-of-two lift after pb-membership; pb-test-load after the census

<details><summary>reasoning, evidence, package</summary>

**The by-hand lift (host, `scripts/cw-rails-lift.sh --sandbox`, 2026-09-26T03:14Z).**
- Steps 1-4: STRIPPED workspace-hack from 9 manifests; closure COUNT 13; no FORBIDDEN and no HAND-SPELLED-PATH; build rc=0 in 39.8 s; test rc=0.
- Step 5: the invite was read from 127.0.0.1:9741's `join_link`. The log reads "join: admitted mesh=Meshsonics", "Joined Meshsonics as cw-rails-lift (node-70b77ed23c804bd0). 13 member(s) on the roster.", and "roster: this node plus at least '6c955b5f1361'".
- It then chose "offering a library: Alexs-MacBook-Pro" and ended VERDICT 0: "GET /v1/mesh/media?peer=Alexs-MacBook-Pro returned no URL: " with an empty reason.
- The daemon's `/v1/mesh/media` lists two offerers, both `"status": "offline"`. The same route answers HTTP 409 `{"error":"'Alexs-MacBook-Pro' is offline — a bridge to it would accept and then never answer"}`, a correct named refusal that `curl -fsS` (cw-rails-lift.sh:465) threw away.
- The member was left on the roster, and `svrn mesh forget-member` matched neither `70b77ed2` nor `node-70b77ed23c804bd0`.

**Why not configure RuggedFox as a media origin so the step passes today.** That would put a test's pass condition on the operator's live node configuration. The mesh-of-two with a fixture origin measures the same route without touching it.

**The flakes.** From 612cdd77b's and 69d30f52e's bodies:
- `local_only_boot.rs:306` and `rails_base_config`'s ring-rail test: red in 3 of 6 TESTALL runs at 10.33-10.35 s, about 1.9 s alone.
- join_child: red in 1 of 5, "Address already in use".
- `corpus_lifecycle::install_pause_resume_lifecycle`: red once at 4.9 s, message lost.
- `.config/nextest.toml` has profiles and slow-timeouts but no `[test-groups]`.

</details>

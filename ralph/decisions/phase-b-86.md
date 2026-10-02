<!-- ledger -->

**phase-b-86 · 2026-10-01 · on-prem works at the end of Phase B · operator, recorded by the seat** — this commit
- Needed: the seat's census found the on-prem kit (sovereign/deploy/onprem) cannot work at the tip. 5cb09f22b deleted sovereign-server, which was FIVE_PROGRAMS §2b's step 5, before step 3 (`Asserted`, loopback grants nothing, corpus grants) existed. Every finding was checked in the tree:
  - Behind nginx on the same host every request is loopback, and client_auth.rs:28-34 admits any loopback caller as the owner.
  - `POST /v1/documents {path}` ingests a server-side path (documents_http.rs:114-120).
  - 7 of the 14 nginx-proxied routes are gone and 3 changed shape.
  - Solve, /mcp, web search, `web_fetch`, `wikipedia_fetch` and `probe_url` are compiled into sovereign-stock unconditionally, though the kit's hardened build had removed them.
  - The kit still builds, installs and starts sovereign-server.
- Chose (operator, 2026-10-01): "I do want on-prem working at the end of this."
  - Identity: API keys now. Each key resolves to `Principal::Asserted { sub, groups }`, loopback grants nothing on a keyed daemon, conversations are scoped by owner, and the corpus allow-list carries over from `[retrieval] corpora`. JWKS/SSO later produces the same principal.
  - API: no deployed client depends on the old shapes. The kit moves to the daemon's API, and only routes with no daemon equivalent are added.
  - The seat's ship-gate recommendation O2 ("retire with a note") is withdrawn.
  - Rows, splits of pb-distribution:
    - -onprem-identity;
    - -onprem-compose: its own distribution binary composing svrn, serve and ingest, without code's face, the mesh or the web tools, and with OCR;
    - -onprem-routes;
    - -onprem-kit: acceptance.sh passing end to end on this host, in a sandbox prefix and on sandbox ports.
  - The ship gate gains P8. These rows and pb-distribution-release-bins run in worktree B beside the main loop's mesh chain (phase-b-79 mechanics).
- Because:
  - The README's promise ("routes that could reach a shell are not in the binary") is kept structurally by a distribution that does not link them (principle 10). Config or nginx alone would leave it remembered.
  - The keyed mode is off on a daemon with no keys, so the desktop and local users see nothing change (principle 6). The one key store and the one `Asserted` decider follow principle 8. Reusing `process::run`'s `Option` seams and the `Withheld` record follows principle 11.
- REVIEW-AFTER: pb-distribution-onprem-kit. This decision is falsified if acceptance.sh cannot pass without restoring an old route shape, which would mean a client did depend on them, or if the onprem binary still contains a withheld tool's strings.

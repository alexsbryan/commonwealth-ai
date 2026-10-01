<!-- ledger -->

**phase-b-82 · 2026-10-01 · pb-mesh-exit-transport (the flip) · seat, amending phase-b-81 fork 4** — this commit
- Needed: phase-b-81 ruled that the DST fault pack is ported onto cw-rails' gossip inside the flip, before landing. It reasoned that the seat's landing condition (a5ff24788) forbids landing while a surviving behaviour has no successor. That pulls ~1,500 test lines and a transport seam into the flip. It also contradicts an existing placement: phase-c's pc-rails-gossip-dst (ralph/next/phase-c/STATE.md:78), staged by pb-mesh-dissolve because "a pack for cw-rails' gossip advances no Phase B finish item".
- Chose:
  - Fork 4: the DST scenarios' successor is pc-rails-gossip-dst. The flip's ledger marks each one "successor owed: pc-rails-gossip-dst". Selection properties specific to the deleted algorithm stay D by name (phase-b-81).
  - The landing condition is clarified. A successor is a named test, or a named open row that an earlier decision already placed it in. A surviving behaviour with neither is still a gap and blocks the landing.
  - Forks 1, 2, 3 and 5 stand as phase-b-81 ruled. LIFT tests re-priced ~+2,400 → ~+900.
- Because:
  - The old pack's subject was sovereign-mesh's gossip round, which the flip deletes. cw-rails' gossip is a different implementation, and pb-rails-parity and pb-rails-membership accepted it without a fault pack. So the flip removes no coverage cw-rails ever had. Writing a pack for it is new work that phase-b's scope freeze (phase-b-32) already placed in phase-c.
  - The seat wrote the condition to stop silent coverage loss (principles 5, 6). A deferral to a named row decided earlier is not silent, and the condition's words said more than its intent.
  - Rotate safety, ring-sync refusals and the wizard join are behaviours of code that survives the flip or moves into cw-rails. Their tests follow that code, so those forks stay in the row.
- REVIEW-AFTER: pc-rails-gossip-dst. This decision is falsified if cw-rails' gossip fails a scenario the old pack covered (convergence, decay without ghosts, skew, partition heal, quiescence, wire faults, seeded soak) in a way a user of the flipped node would have seen.

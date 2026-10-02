<!-- ledger -->

**phase-b-93 · 2026-10-01 · pb-distribution-ship-gate · operator, ruling the ship gate's open decisions O1, O3, O4 and O5 (each as the seat recommended)** — this commit
- Needed: ralph/PHASE_B_SHIP_GATE.md left four operator decisions open with recommendations; the gate cannot give every row a verdict while O3 decides what Tier 1's test inventory accepts, and O1/O5 decide what the merge carries.
- Chose:
  - O1, merge timing: hold the merge until pb-distribution and F1 (pb-distribution-release-bins) are on cut.
  - O3, test deletions: restore `containment_guard_e2e` against the stock binary, and give each of a546a456b's 15 removed tests a named successor or a new one. Row pb-distribution-o3-tests (this commit), which pb-distribution-ship-gate now depends on.
  - O4, debug first token: accept the debug-profile x1.09-1.13 first token on debug hosts; the release bars (phase-b-25) bind.
  - O5, what lands on main: FIVE_PROGRAMS.md, the code-binding decisions and PHASE_B_SHIP_GATE.md land; `ralph/next/*` stays off main.
- Because: the operator chose each recommendation in the seat session of 2026-10-01. O3's reason is the gate's own: the boot guard is a safety property (a SIGABRT of the whole daemon, 2026-07-27, twice), and a removed test with no account is a finding under Tier 1.
- REVIEW-AFTER: the ship gate's Tier 1 reading. Falsified if pb-distribution-o3-tests lands with a blank cell in its census table, or the restored guard e2e does not go red with the guard call removed.

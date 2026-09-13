# A REPRODUCIBLE TRIGGER IS NOT A MECHANISM — and I got this wrong on the record, so it is written down. Corrected 2026-08-06.

A REPRODUCIBLE TRIGGER IS NOT A MECHANISM — and I got this wrong on the record, so it is written down. Corrected 2026-08-06.

WHAT HAPPENED. M6-C drove 8 sequential streaming calls (each ~3,150-token prefill) at LittleMac, the sole holder of Qwen3-4B-Q4_K_M. The peer stopped answering at call 4. Operator authorized a full re-run; it died AGAIN at call 4, ~20 min apart:
  run 1  calls 0-3 TTFT 111.8/143.8/156.6/124.2 s, call 4 no content, Offline 11:29:31 (staleness 74s)
  run 2  calls 0-3 TTFT 108.2/103.5/108.0/110.9 s, call 4 no content, Offline 11:48:59 (staleness 67s)
I reported that as causation established — "one agentic loop takes a thin peer off the mesh" — and an operational ceiling of four ~3k-token prefills.

BOTH OF THOSE ARE RETRACTED. The operator supplied the mechanism: LittleMac was running an OLD DAEMON carrying a known METAL bug and crashed on that. Load was the trigger that exposed an already-broken build, not a capacity limit. Nothing about thin peers in general was measured.

THE LESSON, which is the reusable part: two matched runs failing at the same call index proves the load REACHES a fault and says NOTHING about what the fault is. The originator's view is identical whether the far side OOM'd, overheated, or hit a Metal bug in a stale binary — manifest transport errors to the peer's bridge port, then `gossip: peer marked Offline` on the 60s staleness threshold. ANY PEER-SIDE FAILURE ATTRIBUTED FROM ORIGINATOR-SIDE SIGNALS ALONE IS A COULD-NOT-JUDGE (§18.1), however clean the reproduction looks. Check the peer's OWN log first; it cost ~20 min and two operator interventions to establish a trigger that a glance at the far-side log resolved instantly.

ALSO RETRACTED: the "do not drive sustained load at LittleMac" warning. It was a stale-daemon fault and the operator has fixed it.

WHAT SURVIVES, because it never depended on why the holder died: a named request carries `soft=false`, so when its sole holder disappears the originator REFUSES rather than degrading — no second holder, no local fallback, no retry, the agentic loop stops. Two directions to price: (a) soft-degrade a named request whose holder dies mid-stream (§18.3 — substitution must be NAMED, never silent), or (b) a second holder for anything a consumer pins, which also makes the min-inflight balance real. Also unaffected: the TTFT series as measurements of that build, and run 2's FLAT series (~107.6s mean, 7% spread, identical prefix) confirming no prefix reuse on the peer path (note 5ff2a061).

STALE-BINARY CHECK BELONGS IN MESH TRIAGE: before attributing a peer fault to anything, confirm the peer is running a current daemon. The dispatcher warns about stale siblings locally; nothing surfaces a PEER's build age to the originator.

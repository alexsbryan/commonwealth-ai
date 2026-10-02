<!-- ledger -->

**phase-b-26 · 2026-09-27 · pb-svrn-dials-serve → prove the chat round trip lossless, fix what it drops, re-measure under release · operator** — this commit
- Needed: under release (f12a348fb, n=15×3) the first-token bar missed 2 of 3 runs: ×1.125, ×1.103, ×1.078 against ≤1.10. Embeddings passed 3 of 3. The laps put the wire (transport plus HTTP framing) at about 0.8 ms, 3.5%. The remaining 1-2 ms is inside serve, after the adapter returns its stream: the engine's first token on the request serve rebuilds from the chat wire arrives later than on the daemon's own request.
- Chose (operator, from the seat's escalation): prove the round trip lossless with a golden test and fix any field it loses or changes, then re-measure once under release. The instrument and the bar stay as they are.
- Because:
  - Principle 6: if the round trip changes what the engine receives, every turn on the dialing path runs a request the daemon did not build, a silent substitution beyond latency. The bar caught a difference users would get.
  - Principle 8: one request, carried faithfully, pinned by a golden equivalence test rather than by agreement between two translators.
  - Principle 7: moving the in-process arm onto serve's adapter would change the instrument after a miss and hide that difference, so it was refused. Accepting the miss would ship an unexplained per-request change, so that was refused too.
  - Boundary gate: 49 at f12a348fb. No code is in this commit.

<!-- appendix -->

## phase-b-26 · 2026-09-27 — the dialing path runs the request the daemon built, proved by a golden round-trip test

<details><summary>reasoning, evidence, package</summary>

The package was ralph/next/phase-b/ctl/NEEDS_HUMAN.md at 04:08Z, archived on this host at target/ralph/phase-b/NEEDS_HUMAN-phase-b-26.md when the halt was cleared. It was the first halt marked `operator-only:`. The supervisor logged "operator-only halt — a pre-registered bar a row cannot meet (no resolution session)" and exited without sending a director (2880e5de0).

The worker's candidate fields: sampling, `preferred_speed` / latency class, the stop set, template kwargs. The two translators are `RemoteApiProvider::build_request` (oicp-client lib.rs:595) and `build_completion_request` (serving-host inference_adapter.rs:1377).

What would falsify this:
- The round trip is already lossless and the release miss remains. Then the 1-2 ms is serve's own path (admission, slot pick, runtime). It goes back to the operator with that breakdown.
- A field cannot be carried on the OpenAI chat wire. Then it needs an extension field on serve's loopback wire, which is a wire change the operator sees before it lands.

</details>

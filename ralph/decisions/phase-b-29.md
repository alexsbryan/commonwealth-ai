<!-- ledger -->

**phase-b-29 · 2026-09-27 · Phase B → the stock install is one process; every open row is censused and trialled before more code; a scope guard binds the rest · operator**
- Needed: the seat's post-mortem of 2026-09-23..27 counted 100 halts and decisions: 31 false row premises and 27 wire or lifecycle surprises, from rows written off the boundary gate's edge model instead of their use sites. pb-svrn-dials-serve alone took 52 work commits, 6 decisions and 18 amendments (212 → 4,703 words), and minted five copies of serve's state, all to run the stock install as two processes, which FIVE_PROGRAMS §2c:149 asserts and phase-b-1:24 never intended. The operator: "systematize this move to disambiguate as much as we can ahead of time and just ruthlessly execute to the end goal".
- Chose (operator, from the seat's recommendations):
  - Q1. The stock install is ONE process. A `[[distribution]]` binary builds serve's provider in-process, hands it to svrn through the `InferenceProvider` port (sovereign-contracts), and binds serve's router on serve's port, so cw-rails, code's FIM and cli-llm still dial it. The dialing path pb-svrn-dials-serve built stays for a standalone or remote serve.
  - Q2. Placement and serve's lifecycle belong to the distribution. svrn dials a configured base and reports an absence; it brings nothing up.
  - Q3. `PeerTransport`, `PeerContact` and `TrafficClass` move to a client half beside sovereign-turn-client (quality/ARCH_LAYERS.toml:1689, :1695).
  - Q4. A distribution is a `[[distribution]]` row outside every package: the dispatcher, the setup verb, the service installer and the stock binary. serve's library face is `sovereign_serve::bundles` plus the serving assembly's entry. The gate support lands before pb-serve-package.
  - Q5. There is no phone host build. The phone is a client (§2a, §5).
  - HUMAN-pb-lanes-dials-serve is re-registered as a same-client before/after on this host; its "after" reads the stock binary.
  - REVIEW-pb-preflight-1..3 census and trial every open row before any more code, and end in ONE package holding every remaining operator fork. A scope guard binds every row after them.
- Because:
  - Principle 12 and phase-b-1's own intent: the program boundary is held at compile time by the gates and the lifts, not by the process count. The pid record, the reload cell, the port override, the NER probe and the five state copies exist only to keep two processes agreeing.
  - Principle 8: FIVE_PROGRAMS and the decisions were two deciders of the stock placement. §1, §2c and §4 rule 2 now follow phase-b-1, in this commit.
  - Principle 11: REVIEW-pb-census re-read the model instead of trying the moves, and 9 of phase-b's 14 worker halts were then found at execution. A trial of each move finds them before.
  - Boundary gate: 49 at 2e96257bf. No code is in this commit.

<!-- appendix -->

## phase-b-29 · 2026-09-27 — one process for the stock install, and the remaining forks answered up front

<details><summary>reasoning, evidence, package</summary>

The escalation was the seat's, in session after pb-svrn-dials-serve closed (2e96257bf) and the loop halted at HUMAN-pb-lanes-dials-serve (07:29Z). Three read-only agents built the evidence:

- Halts and decisions, 09-23..27: 100 events (93 decision files, 7 halts without one). By root cause: a row premise the code contradicted 31; lifecycle or ownership between processes 15; scope or row size 14; instrument 12; a wire or boundary contract left unstated 12; the loop's tooling 9; an operator change of direction 5. Directors cleared in a median 2.5-4 min; the loop waited about 22 h on operator decisions.
- Seat catches: 50, led by duplicate deciders or copies of another process's state (9), tests that could not fail (9) and tracing allowlists (8). The same classes recurred on later rows after being caught; none was caught by a gate.
- Open questions: Q1 reshapes six rows directly (HUMAN-pb-lanes-dials-serve, pb-serve-distributes, pb-serve-ranks, pb-serve-package, pb-distribution, pb-daemon-adopts) and three conditionally (pb-mesh-exit-mesh, pb-cli-llm, pb-meshapp-rest). Q3 blocks five mesh rows under either Q1 answer.

Checked in this session: phase-b-1.md:24 ("a one-process stock install") and :76 (the loopback-bar falsifier names in-process placement as the operator's); FIVE_PROGRAMS.md:25-27, :148-150 and §4 rule 2 before this commit; the eval's chat and embeddings go through the daemon's HTTP (sovereign-cli-llm chat_cmd/bootstrap.rs:122-129), so the lanes see a daemon-side serving change; quality summary `est_secs` put retrieval-prod at ~90 s and synth at ~330 s.

HUMAN-pb-lanes-dials-serve, bars set before any data:
- One client, two servers: both readings use the eval binary built at e201c7372.
- Before: the deployed pre-switch in-process daemon (pid 2280388). retrieval-prod x2, synth x3.
- After: the stock one-process binary on this host.
- retrieval-prod identical per question. synth after-mean within 0.05 of before-mean; a before spread above 0.06 is could-not-judge. A miss goes to the operator with per-question diffs, 51b547655's single-turn flatten change first.
- The July and August baselines are not the comparison: they fold weeks of drift into the reading.

The sweep's operator forks include the five smaller gates the scout found (pb-ingest-rehome's reader leaf and grants types, corpus-mcp's membership, pb-mesh-dissolve's unowned modules, pb-code-server's `mcp` dependency, the host-kit cap).

Rejected: keeping two processes for the stock install (the cost is measured above, and debug builds, which this host deploys, miss both latency bars at x1.09-1.13); answering the smaller gates now without the census facts (that is the habit this decision ends).

What would falsify this:
- The stock binary cannot host both programs' bundles without a second copy of a §2c drive. Then the placement is wrong: return with the trial.
- A sweep finds an open row whose outcome needs serve in its own process. Then Q1 is revisited for that outcome.
- After the forks are answered, a row still halts at census on a premise its trial would have caught. Then the census rule is not being applied, and the director charter says why.

</details>

<!-- ledger -->

**phase-c-10 · 2026-10-02 · pc-deployed-turn-latency · director** — this commit
- Needed: the lane's package (phase-c-9) asked what happens to the 316 `dr-estate-dr-*` deep-research run corpora, which every unscoped turn searches and which add nothing to the answer. It offered three choices: uninstall them (A), file a phase-d row (B), or rule them needed (C). It also asked that three findings be filed.
- Chose: B. pd-scoped-run-corpora files the behaviour change for the operator's phase-d. The row closes as attributed: every stage is named, and the one cost that is neither needed nor removed is filed with its number. A stays open to the operator. The e2e-lane finding becomes pd-e2e-whole-turn, and the silent atlas spans become pc-atlas-grounding-silent-spans below the cut line.
- Because: A deletes user data, and the charter leaves that and any irreversible step to the operator. Leaving run corpora out of the unscoped fan-out changes behaviour beyond what the row states ("Leave these for the operator"), and the charter sends that to phase-d as a filed `pd-` row ("A new finding is filed, never added above the line"). C would rule a measured 6.4 s of search for 0 survivors needed, and the evidence says otherwise. REVIEW-AFTER: closing a row whose outcome says "needed or removed" with one cost filed rather than removed is a reading of the outcome, not its letter.

<!-- appendix -->

## phase-c-10 · 2026-10-02 — the deployed turn's dr-estate cost goes to phase-d; the row closes as attributed

<details><summary>reasoning, evidence, package</summary>

The package's premise was re-read, not taken on trust. At 2026-10-02 on RuggedFox, `GET /v1/corpora` on :9741 returned 380 corpora, 316 of them with ids starting `dr-estate-dr-`. `ls ~/.sovereign/indexes` held 2,189 entries, 317 of them `dr-estate-dr-*`. The stage table, the per-corpus sums (dr-estate 6.4 s of 16.2 s, 0 of 20 survivors) and the clean-root comparison are the lane's phase-c-9 (25e3b4871), with raw files under the lane's target/ralph/phase-c/census/. I did not re-run the turns. The package's own n=3 readings were taken at loads of 2.4-2.9, and nothing in this decision depends on a number finer than "most of the fan-out's corpora, none of the answer".

Filed:
- pd-scoped-run-corpora (phase-d): per-run deep-research corpora stay out of the unscoped fan-out and out of the router's corpus list unless the turn names them.
- pd-e2e-whole-turn (phase-d): the throughput lane's e2e arm times the whole turn, and it either holds "no corpus attached" or says it doesn't.
- pc-atlas-grounding-silent-spans (cleanup, below the cut line): the two ~3.7 s atlas-grounding spans get an event each.

What would falsify this: the operator ruling that run corpora are meant to be searched on every turn (then C, and pd-scoped-run-corpora is struck), or a deployed turn with the dr-estate corpora removed that is still slow for a reason the phase-c-9 table did not name.

</details>

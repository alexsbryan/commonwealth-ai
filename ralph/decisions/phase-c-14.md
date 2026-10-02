<!-- ledger -->

**phase-c-14 · 2026-10-02 · pc-solo-durable · worker** — this commit
- Needed: the row leaves the shape open: append before the ack in solo mode, or ack only what the pump has appended.
- Chose: in every mode, a kv door that changed the store drains the outbox onto the journal before it answers (`KvHost::journal_outbox`, one `drain` lock shared with the tick); the tick keeps the seal check for every namespace either drain fed. No solo branch.
- Because: one mechanism, one decider (principles 8, 10). A meshed node has the same window: a peer only receives a write after its journal append, so "a peer copy" never covers an unjournaled write. A solo-only branch would keep a second ack path alive for no reader. A write deferred for want of a roster (a joining node) stays queued and is acknowledged as before, named at debug.

<!-- appendix -->

## phase-c-14 · 2026-10-02 — kv doors journal before they answer, in every mode

<details><summary>reasoning, evidence, package</summary>

Cost, read in both directions on `tests/kill_durable.rs` (50 door writes, sovereign-vulkan toolbox, load average 2.3, three runs each):

- journal before ack: p50 695 / 527 / 968 us, p95 1065 / 881 / 1629 us; the kill test passes 3/3.
- ack before append (PLANT): p50 210 / 192 / 202 us, p95 646 / 505 / 699 us; 3/3 red, `kv-durable/0: an acknowledged write was lost: null`.

So about +0.3 to +0.8 ms per write at p50 on a near-empty journal. The cost grows with the journal, because every `RingJournal::append` re-reads it (kv.rs `run_forever`'s doc). One run at WRITES = 1,000 per namespace (load 2.5): fix p50 7,205 us, p95 13,726 us, max 25,335 us (16.2 s for 2,000 writes); plant p50 209 us, p95 255 us (0.59 s), red. A sequential bulk import through the door now pays O(journal) per row until the tick's seal bounds the journal (`SEAL_AFTER_OWN_OPS` = 2,000 own ops). Making the append itself cheaper is the rail crate's, not this row's.

</details>

<!-- ledger -->

**phase-c-1 · 2026-10-01 · pool dispatch (first wave) · director** — this commit
- Needed: the pool halted before its first wave: `pool: probe claude-opus-5-5 — names no provider/model pair — not probed` (ralph/log-phase-c.txt, 2026-10-02T02:46:04Z). The roster probe (1e14de34d) assumed opencode's `provider/model` grammar, but phase-c's `worker_bin` is the claude shim, which takes bare ids. The serial loop never probes at dispatch, so phase-b and pc-pool-ready never met it.
- Chose: `probe_refusal` probes a bare id through the client when the queue declares its own `worker_bin`; with no declared client (opencode), a bare id is still refused. The loopback refusal is unchanged. No row or queue edit.
- Because: the model-name grammar belongs to the client that takes the name (principle 12), not to the probe. Rewriting the roster to a fake `anthropic/claude-opus-5-5` would hand the shim an id `claude -p --model` does not know. Boundary gate: unchanged, no Rust in this commit.

<!-- appendix -->

## phase-c-1 · 2026-10-01 — the dispatch probe takes a bare model id when the queue declares its own client

<details><summary>reasoning, evidence, package</summary>

Reproduced: `scripts/ralph.py:905` returned the refusal for any id without `/`; `_dispatch_model` (ralph.py ~1860) turns a refusal for every roster entry into the halt. After the fix, `probe_model('claude-opus-5-5', paths_for(pool --queue phase-c))` returned `(True, '')` through `scripts/ralph-claude-shim.sh` against the live plan. New test `ProbeTests.test_a_declared_worker_bin_probes_a_bare_id_through_itself` fails with the old ralph.py (FAILED failures=1) and passes with the new; the full `scripts/tests/ralph.py` is 158 tests OK.

Falsified if: a queue that declares a worker_bin which does route through opencode config (a wrapper around opencode with a loopback provider) gets probed against the mesh daemon. Then the loopback check must consult the client, not the id's shape.

</details>

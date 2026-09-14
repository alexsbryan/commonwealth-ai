# SHRINK-FAST-PRUNE IS ITSELF AN ABORT FACE — live capture 2026-07-27 23:13 (RuggedFox, daemon pid 118151, systemd exit 134/SIGABRT).

SHRINK-FAST-PRUNE IS ITSELF AN ABORT FACE — live capture 2026-07-27 23:13 (RuggedFox, daemon pid 118151, systemd exit 134/SIGABRT).

Chain, verbatim from journalctl --user -u sovereign.service:
- 23:01:25 worker BeefyMac (<lan-ip>:50052) discovered → reload_primary → 122B loaded distributed, mode=distributed total_blocks=48 local_blocks=36, worker holds 12 (SOVEREIGN_RPC_BLOCK_SPLIT=12,36 pinned).
- 23:13:29 worker-eligibility: Eligible → Absent (flaps=1) → eligible set empty → `RPC worker set changed — reloading primary to redistribute workers=[]` → `reload_primary: redistributing across updated RPC device set`.
- 23:13:29 ggml-rpc.cpp:386 `Remote RPC server crashed or returned malformed response` → GGML_ABORT → ggml's handler shells out to gdb and dumps a full LWP backtrace into the journal (that gdb noise IS the abort handler, not an operator attaching).
- 23:13:47 sovereign.service Main process exited status=134. systemd Restart=on-failure brought it back at 23:14:32, clean, no 122B resident.

THE POINT: bootstrap.rs's shrink-fast-prune exists to drop a dead worker's device BEFORE it aborts the host mid-compute. But pruning is implemented as `reload_primary()`, and the reload's teardown of the old sharded model must free buffers ON THE DEAD WORKER — so the protective path is itself a guaranteed abort when the worker is already gone. No inference was in flight; this is the teardown face (:386, sibling of the :379 face in note 7204c7f8), not the mid-decode face (:491).

CONSEQUENCES FOR THE COMPUTE-CHILD ARC (plan let-s-plan-out-our-velvet-patterson):
1. Worker-set change MUST be "kill the child, spawn a fresh one with the new worker list" — never an in-child graceful reload. A graceful reload against a departed worker is exactly this abort; in a child it would just be a contained crash, but a respawn is faster and deterministic. Supervisor terminate() = SIGTERM → bounded grace → SIGKILL, which is correct for a child wedged in ggml's abort handler (that handler blocks for seconds shelling out to gdb).
2. Suppresses the abort at its real source: the abort is in the process that HOLDS the sharded model, so moving the load into the child moves BOTH faces (decode + teardown/prune) out of the daemon.
3. Do NOT "fix" this in-process by trying to free the dead device first — there is no error path in ggml's RPC client; every call against a dead endpoint aborts.
4. GGML_ABORT's gdb backtrace dump means an aborting child takes ~3-15s to actually die; the supervisor's grace window must tolerate that (it does: bounded grace then SIGKILL).

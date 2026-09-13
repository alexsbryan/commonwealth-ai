# DISTRIBUTED FAULT-TOLERANCE TEST TOOK DOWN THE WHOLE DESKTOP SESSION (2026-07-27 19:27–19:28, RuggedFox/fedora) — post-mortem, machine…

DISTRIBUTED FAULT-TOLERANCE TEST TOOK DOWN THE WHOLE DESKTOP SESSION (2026-07-27 19:27–19:28, RuggedFox/fedora) — post-mortem, machine never rebooted (uptime survived; "crash" was GNOME session collapse).

Causal chain, each link verified from coredumps + journal:
1. 19:27:06 fault injection killed the remote shard worker mid-decode. ggml's RPC client has NO error path: `recv failed (bytes_recv=0)` → `ggml_abort` inside `ggml_backend_rpc_buffer_get_tensor` → SIGABRT of the ENTIRE sovereign-cli-daemon (1 GB core, stack: llama_decode → ggml_backend_sched_graph_compute_async → rpc_buffer_get_tensor → ggml_abort). A dead worker is structurally fatal to the host process under ggml-RPC. Same signature at 18:21, 18:34, 19:27 (each 1G core).
2. Daemon auto-restarted (~19:27:50). All peers unreachable (BeefyMac transport error, iroh dials timing out — peers were part of the fault test), so mesh-inference fell through to "serving complete() locally" and resolved commonwealth/primary → Qwen3.5-122B-A10B-UD-Q5_K_XL, LOADING THE FULL 122B LOCALLY. RSS hit 90,924 MB (container peak 115.8G + 7.5G swap).
3. 19:28:31 gnome-shell: `amdgpu: The CS has been rejected (-12 = ENOMEM)` → SIGABRT. Strix Halo unified memory: the 122B load starved the compositor's GPU/GTT allocations. Session hit exit.target; systemd tore down user.slice, SIGTERMed daemon, killed toolbox container — everything the operator saw as "the whole machine crashed."

Invariants this reconfirms/creates:
- (project_bigmodel_local_load_freeze) local-loading the 122B on this box freezes/kills the session — the NAMED-MODEL/peer-unreachable fallback path must NEVER resolve to a local load of a model that doesn't fit comfortably beside the desktop. The 36GB-class RSS guard must gate the fallback path too, not just direct loads.
- ggml-RPC host survival requires process-level isolation of the sharded decode (sovereign-compute child boundary) OR an upstream patch: get_tensor failure aborts, it does not return an error.
- Benign noise in the same window: the all-day 3.1M sovereign-cli-daemon SIGABRTs were `--compute-child --role mock --name crashslot` — the harness's own deliberate crash children, not a defect.

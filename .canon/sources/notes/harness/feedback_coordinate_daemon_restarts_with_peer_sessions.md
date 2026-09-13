# Another Claude session usually shares this machine and the :9741 daemon; message it (ListAgents/SendMessage) before any daemon…

Operator direction 2026-09-08: there is normally another agent session on this machine (found via `ListAgents`, e.g. `commonwealth-ai-e2`) using the same sovereign daemon on :9741. Coordinate daemon restarts with it over `SendMessage` before stopping the daemon, and tell it what build it will come back on.

Why: On 2026-09-08 a `sovereign daemon stop` landed while the daemon was mid chat turn with judge calls in flight (a quality lane or peer-driven question), cutting that run. A `pgrep` for local bench/quality processes showed nothing because the driver was another session or a tailnet shell, so process inspection is not enough evidence that the daemon is idle.

How to apply: Before `sovereign daemon stop|start|restart`, run `ListAgents`, message every busy peer session with the planned window and the new build, and hold if they ask. Also check the daemon log tail (`~/.svrnmesh/logs/daemon.err`) for in-flight turns. Note that `daemon start` under launchd may report "didn't respond within 120s" while the job is simply loading models; `launchctl print gui/$UID/com.svrnmesh.daemon` shows the real state. Related: [[long-runs-launchd-relaunch-dont-ask]], [[fanout-one-dod-sweep]].

# daemon RSS hard limit (self-SIGTERM) now DEFAULTS OFF; opt in via SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>|auto — only meaningful under a…

Fixed 2026-07-18: `memory_watch.rs` `hard_limit_policy` in sovereign-cli-daemon now returns `None` (DISABLED) when `SOVEREIGN_RSS_HARD_LIMIT_MB` is unset. Previously it defaulted ON at a RAM fraction (65% macOS / 85% Linux) — which self-SIGTERMed the daemon (exit 102) expecting a supervisor to relaunch. On a bare `sovereign daemon start` (unsupervised), nothing relaunched → the daemon just went DOWN. On a 64GB Mac the hard limit was 42598 MB (65%); loading a 35B primary (32.6GB) + fast (4.6GB) + embed pushed RSS over it and killed the daemon with no recovery.

Why: a self-kill hard limit is only useful under something that restarts you. The code had drifted from the spec — `DAEMON_RESILIENCE.md` already said "hard limit disabled unless env set — only daemon-supervised.sh sets it; systemd/launchd units set no env." The code now matches the spec.

How to apply / current behavior:
- Unset / garbage / `0` / `off` → hard limit DISABLED (default).
- `SOVEREIGN_RSS_HARD_LIMIT_MB=<positive mb>` → that explicit ceiling.
- `SOVEREIGN_RSS_HARD_LIMIT_MB=auto` → RAM-derived (65% macOS / 85% Linux, the old default) — for supervisors that want RAM-aware.
- `scripts/daemon-supervised.sh` sets it explicitly (36000); launchd/systemd units set no env → disabled (rely on soft-warn + OS).
- Soft limit unchanged — still default ON (50% macOS / 70% Linux), warn-only/non-fatal; the observability signal. `doctor` probes RSS vs the SOFT limit.
- Boot log: `memory-watch: armed` (no `hard_limit_mb` field when off) + `memory-watch: hard limit disabled (default; unsupervised daemon)`.

LIVE-VERIFIED on BeefyMac 2026-07-18: after redeploy, boot log shows hard limit disabled; woke the 35B → daemon healthy at 41.4GB RSS with all 3 models resident, no self-kill (previously self-terminated at 42.6GB). Test `hard_limit_defaults_off_unless_opted_in` (memory_watch.rs).

Part of the same 2026-07-18 session as [[project_model_config_source_of_truth_2026_07_18]] (surfaced while live-verifying the residency endpoint). UNCOMMITTED.

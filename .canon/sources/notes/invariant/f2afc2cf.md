# THE DAEMON IS THE DESIGNATED OOM VICTIM ON THIS HOST — root-caused from the kernel log, 2026-08-24 (supersedes the "my cargo builds did it"…

THE DAEMON IS THE DESIGNATED OOM VICTIM ON THIS HOST — root-caused from the kernel log, 2026-08-24 (supersedes the "my cargo builds did it" half-explanation in note 849417cd).

`journalctl -k`: FOUR global kernel OOM kills between 20:52 and 23:44, restart counter at 6.
  Out of memory: Killed process <pid> (sovereign-cli-d)
    anon-rss: 52.8GB / 39.1GB / 51.1GB   total-vm: 97-122GB   oom_score_adj:200
`constraint=CONSTRAINT_NONE ... global_oom` — the BOX runs out, not a cgroup limit. The daemon carries oom_score_adj:200, i.e. it is deliberately made the most killable process, so it dies every time regardless of who allocated.

NOT ONLY MY BUILDS. Three kills coincided with cargo gates I was running (see 849417cd, still true and still a rule). The 23:44 kill had NO build running and 77GB free afterwards — `tailscaled` invoked the killer and the kernel picked the daemon. So a resident 27B at ~51GB RSS plus normal desktop load is already near the edge on a 125GB box.

LIKELY AGGRAVATOR, NOT YET PROVEN: `models.context_size` 32,768 -> 65,536 was applied 2026-08-23 (pre-registration "INSTRUMENT AMENDMENT"). That doubles the KV allocation on a 27B and is the newest change in the memory picture; these OOMs began the day after. Worth confirming before the next long run — a run lost at hour two is expensive.

CONSEQUENCES FOR EVERY LONG SCRIPT:
 - Gate on daemon readiness AND re-warm the judge after any gap: a restart evicts the loaded model.
 - RETRY ON ANY NON-SCORE, never on a matched error string. The A/B's first pass lost control-1 because its predicate matched only "Connection refused|URLError" while the real error was RemoteDisconnected("Remote end closed connection without response") — so a transport failure was recorded as a finished attempt. Invert it: a run counts as scored ONLY if its record contains an OVERALL line; anything else retries (§18.3, four verdicts not two).
 - `SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>|auto` exists and is OFF ("memory-watch: hard limit disabled (default; unsupervised daemon)"). Under systemd Restart=on-failure it WOULD give a clean self-exit instead of a mid-request SIGKILL. Not enabled tonight — it needs a restart, and a restart mid-flight is the thing we are trying to avoid.

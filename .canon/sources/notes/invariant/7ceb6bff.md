# THE DAEMON'S ANON GROWTH UNDER LOAD IS A STAIRCASE, NOT A LEAK AND NOT A PLATEAU — measured RuggedFox 2026-09-12 over 74 min of chaos soak,…

THE DAEMON'S ANON GROWTH UNDER LOAD IS A STAIRCASE, NOT A LEAK AND NOT A PLATEAU — measured RuggedFox 2026-09-12 over 74 min of chaos soak, 2-min sampling (test-artifacts/sweep-2026-09-12/daemon-mem.tsv).

THIS SETTLES THE CONTRADICTION between note d1f61249 (2026-07-10: "NOT a leak — high-water-mark retention, bounded by the largest prompt seen", measured flat across repeats) and note 401428eb (2026-08-26: "the daemon's anon set grows ~62 MiB/min under synth load", measured over 10 min and explicitly left unsettled). BOTH SAW A REAL LIMB OF THE SAME CURVE. Neither window was long enough.

THE CURVE (daemon RSS, GB):
  21:15 15.99 · 21:27 16.57 · 21:39 17.48 · 21:51 18.00 · 22:03 18.32
  22:16 18.31 (FLAT 8 min) · 22:28 18.82 · 22:29 OOM-KILLED at 19.77

Rises in steps with genuine plateaus between them. Net ~3.1 GB/h that does NOT stop within 74 min. My first reading called the 8-minute flat at 22:16 a ceiling — it was one inter-step plateau. A short sample can "prove" either verdict; do not judge this on less than ~75 min.

CONSEQUENCE, THE OPERATIONAL NUMBER: at this slot config (primary 35B-A3B-Q6 + fast 4B + fast_short + embed, context_size=65536) on a 128 GB Strix Halo, A SOAK SURVIVES ABOUT 75 MINUTES. Both of tonight's kills fit: `Out of memory: Killed process ... (sovereign-cli-d) anon-rss:12565656kB` at 20:15 and `anon-rss:19770868kB` at 22:29, both `global_oom`, both with the daemon at oom_score_adj:200. GTT stayed PINNED at 82.87 GB throughout — the growth is host anon, not GPU.

THE DEFENSE IS IN THE REPO AND WAS UNARMED BOTH TIMES. The journal printed `memory-watch: hard limit disabled (default; unsupervised daemon)` at every boot. scripts/daemon-supervised.sh + SOVEREIGN_RSS_HARD_LIMIT_MB turns each step into a drain-and-relaunch instead of a mid-request SIGKILL. Notes f2afc2cf, 39a0674f and 421b9def all say this; nothing has armed it. A hard limit near 18 GB matches the measured staircase on this host.

# AN EVAL RUN AGAINST A WATCHED-FOLDER CORPUS WITH THE SWEEPER LIVE IS NOT A MEASUREMENT — IT SILENTLY SCORES 2.6x WORSE AT THE SAME LATENCY.…

AN EVAL RUN AGAINST A WATCHED-FOLDER CORPUS WITH THE SWEEPER LIVE IS NOT A MEASUREMENT — IT SILENTLY SCORES 2.6x WORSE AT THE SAME LATENCY. Observed 2026-08-02 on `obsidian-vault-959ee8a8f330`.

THE NUMBERS. Same corpus, same command (`svrn eval run --bank sovereign/bench/obsidian/questions.toml --prod-pipeline`), same binary, minutes apart:
    sweeper LIVE:   facts 22/68 (32%)  sources 1/12 ( 8%)
    sweeper PAUSED: facts 58/68 (85%)  sources 8/12 (67%)
Four of twelve questions returned ZERO facts in the live run and full marks in the paused one.

THE PART THAT KILLS THE OBVIOUS EXPLANATION. Per-question search LATENCY was nearly identical across both runs — `ostrom_nested_governance` 13682ms scoring 0/6, then 13416ms scoring 6/6; `ostrom_valencia` 29262ms 6/6 then 27938ms 6/6. So this is NOT CPU contention and NOT timeouts: the queries took the same time and returned different content. The corpus was being MUTATED underneath the reader — `_watched_folder_state.json` was written at 20:28, mid-eval, and a sweep rewrites chunks.lance while queries are hitting it.

WHY IT IS DANGEROUS RATHER THAN MERELY ANNOYING. There is no error, no warning, and no latency signal. The run exits 0 and prints a well-formed report. Nothing in the output distinguishes a contaminated measurement from a clean one. An operator diffing two arms of an ablation would attribute the entire 2.6x to their intervention.

THE PRECONDITION. Before ANY timed or scored run against a watched-folder/vault corpus:
    svrn corpus watch-pause <id>      # verify: watch-status reports paused_manual
    ... run ...
    svrn corpus watch-resume <id>
`bench vault-report` already guards this — it refuses a live-daemon watched target unless `--allow-watcher` is passed. `eval run` has NO equivalent guard, which is the gap that produced this. A guard on the eval side (refuse, or at minimum stamp the sweeper state into the report) is worth adding.

I FIRST BLAMED CPU CONTENTION and re-ran instead of instrumenting — ARCH_PRINCIPLES §0.4. The timing data that refutes contention was already in the output I had printed.

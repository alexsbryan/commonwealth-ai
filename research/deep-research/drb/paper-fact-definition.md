# FACT citation metrics — the paper's definition (vendored)

Source: "DeepResearch Bench: A Comprehensive Benchmark for Deep Research
Agents" (arXiv 2506.11763, v2, via ar5iv), Appendix E "Detailed Calculation
of Citation Metrics". Text vendored verbatim from the paper's appendix body;
the LaTeX formulas are transcribed in the markup below. This file is part of
the frozen DRB subset (research/deep-research/drb/); it is never edited.

## E.1 Citation Accuracy (C. Acc.)

Let `T` denote the set of all tasks in the benchmark, and `|T|` the total
number of tasks. For each task `t ∈ T`:

- Let `U_t` be the set of unique statement-URL pairs extracted for task `t`
  after the deduplication process.
- Let `N_u,t = |U_t|` be the total number of unique statement-URL pairs for
  task `t` that undergo support judgment.
- For each statement-URL pair in `U_t`, a support judgment is rendered, which
  can be either 'support' or 'not support'.
- Let `N_s,t` be the number of statement-URL pairs that are judged as
  'support' for task `t`.

Citation Accuracy (C. Acc.) is calculated by first determining the proportion
of 'support' statement-URL pairs for each individual task, and then averaging
these per-task accuracies across all tasks. The accuracy for a single task
`t`, denoted `Acc_t`, is:

    Acc_t = { N_s,t / N_u,t   if N_u,t > 0
              0               if N_u,t = 0 }                          (4)

This definition ensures that tasks for which the agent produces no citable
statements (i.e., `N_u,t = 0`) contribute an accuracy of 0 to the overall
average, reflecting a failure to provide supported information for that task.

The overall Citation Accuracy is then computed as the average of these
per-task accuracies over all tasks:

    C. Acc. = (1/|T|) * sum_{t ∈ T} Acc_t                              (5)

## E.2 Average Effective Citations per Task (E. Cit.)

Average Effective Citations per Task evaluates, on average, how much useful
and relevant information the agent retrieves and correctly supports with
evidence for each task. It is computed by summing the total number of
'support' statement-URL pairs across all tasks and then dividing by the total
number of tasks in the benchmark.

## Named implementation note (frozen)

The paper's appendix text describes a two-verdict judgment ('support' /
'not support'). The official implementation shipped with the benchmark
(`utils/validate.py` + `utils/stat.py`, vendored in `vendor/utils/`) uses a
three-verdict judgment — `supported` / `unsupported` / `unknown` — and the
leaderboard's published `citation_accuracy` values are produced by the
vendored `stat.py` pooled definition:

    valid_rate = total_valid / total_citations

where `total_citations` counts only non-`unknown` verdicts (unknowns are
dropped from both numerator and denominator) and `total_valid` counts
`supported` verdicts. A `validate_error` (judge failure after 3 retries, or a
missing reference) also drops the row. Cross-check: the vendored submission's
`results/fact/claude-3-7-sonnet-latest/fact_result.txt` reports
`valid_rate: 0.8731742073387959` and the official leaderboard lists
`claude-3-7-sonnet-with-search` at `citation_accuracy = 87.32` — the two
agree on the pooled definition (0.8732 -> 87.32).

This measurement uses the vendored implementation's pooled definition as the
primary statistic (that is the definition the published reference values are
on), and reports the paper's per-task-mean definition (Eq 5) as a descriptive
secondary line. A task with `N_u,t = 0` contributes nothing to the pooled
statistic under the vendored convention (the task's row is skipped), whereas
Eq 5 scores it 0 — the two definitions are both reported, the comparison
against the leaderboard references is made on the pooled definition only.

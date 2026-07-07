# SOLVE × Playwright — UI goals through the same two fields

Status: designed 2026-07-07, not started. Extends `SOLVE_UX.md`
(built + live-verified); no changes to its contract.

## The promise, unchanged

```
solve(webapp_dir, "clicking Save shows a confirmation toast")
```

Same two fields, same job, same streamed rounds, same exits. The
only difference is what "test" means: a Playwright spec driving a
real browser against your app. If you have a failing e2e test, solve
drives it green; if you don't, solve writes the one failing spec
that pins your goal, then makes it pass.

## What makes UI solving clean (the three insights)

1. **The model steers by the accessibility tree, not pixels.**
   Playwright failures come with text the loop already knows how to
   feed back: locator timeouts, `expect()` diffs, and (1.49+) the
   error-context aria snapshot of the page. A language model can't
   see a screenshot; it reads an aria tree fluently. Failure feedback
   = error text + aria snapshot, nothing binary.
2. **Your `playwright.config` already solves app startup.** The
   standard `webServer` block (start the dev server, wait for the
   port) is the precondition — solve adds nothing. If `npx playwright
   test` works for you, `solve` works for you.
3. **Honest fitness needs `retries=0`.** Playwright retries mask
   flake; under strict-improvement gating a flaky pass would promote
   a junk edit. Solve runs the suite with retries off — a flaky test
   reads as failing, which is the truth the loop needs.

## What gets built

- **Detection**: `playwright.config.{ts,js}` → `Framework::Playwright`,
  default command `npx playwright test --reporter=line --retries=0`.
  When a project has BOTH a unit framework and Playwright, unit stays
  the default and `detected` says so; steer to e2e explicitly with
  `--suite e2e` (CLI) / `test_command` (API). No guessing which suite
  a goal means.
- **Parser**: the line reporter's `N passed` / `N failed` summary +
  `✘ [project] › file:line › title` failure lines → pass/fail/total
  and failed names. Suite-level errors (config broken, webServer
  died) count as failing entries with marked names — same
  ran-and-broke rule the other parsers adopted.
- **Feedback**: append the error-context aria snapshot (when
  Playwright wrote one) to the test tail the model sees. This is the
  give-the-model-eyes move, in text.
- **Pin template**: `write_failing_test` gains a Playwright spec
  template — `tests/e2e/pin.spec.ts`, `page.goto('/')`,
  role/text-based locators (`getByRole`, `getByText` — robust and
  idiomatic for small models), one focused expectation from the goal.
- **Browser-scale trial profile**: browser suites cost seconds-to-
  minutes per run, and K parallel candidates each spawning browsers
  would stampede a laptop already running local inference. Playwright
  trials run K=3 candidates, 300s per-candidate test timeout,
  `--workers=1`.

## Done means

1. A demo web app (vite + playwright, committed as holdout-style
   material) with one failing e2e test: `solve` fixes it through the
   daemon surface, rounds streaming.
2. Same app, tests green, UI goal in plain language: solve pins a
   failing `pin.spec.ts`, then drives it green.
3. The quickstart for a web developer is byte-for-byte the same
   two-field call as everyone else's; `detected` line reads
   `playwright · npx playwright test --reporter=line --retries=0`.

## Non-goals (v1)

- Screenshot/pixel assertions and visual diffs — the loop is text.
- Multi-browser matrices during rounds (one project during search;
  run the full matrix yourself before merging).
- Auto-picking e2e over unit when both exist — explicit beats
  magical until receipts say agents get it wrong.

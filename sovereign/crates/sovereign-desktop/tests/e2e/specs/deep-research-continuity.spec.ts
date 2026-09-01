// SPDX-License-Identifier: AGPL-3.0-or-later
// Deep research — the run outlives the view, and says so.
//
// A deep-research run is the longest-lived thing in this app, and it was the
// only one whose progress could not survive a click. Driven with Playwright
// against the pre-change build, the reproduction was:
//
//   start a run → emit round 2 → "Back to chat" → emit round 3 → return
//   ⇒ an EMPTY COMPOSER. Round, gaps, budget and the question itself gone,
//     the backend still working, the shelf listing the live run as
//     `interrupted` with a Resume button beside it, and the finished report
//     fired into a listener that had already been torn down.
//
// Every test below is one clause of the two promises this surface now makes:
// the run ends when the operator says it does (or when it finishes), and at
// no point is the operator left guessing whether it is still working.
//
// The Tauri runtime is mocked (fixtures/tauri-shim.js): `dr_start` returns a
// handle whose channel the spec drives with `emit`, so these tests pin the
// UI's contract against the same event surface the Rust driver emits.
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { DeepResearchRunProgress, DrReport, DrRunSummary } from "../../src/lib/types";

const CHANNEL = "deep-research://progress/job-1";
const QUESTION = "What are the tradeoffs of sodium-ion batteries?";

const CAPS = {
  cli_path: "/usr/local/bin/sovereign",
  flags: ["--run-dir", "--max-rounds", "--corpora", "--consent", "--resume"],
  error: null,
};

const REPORT: DrReport = {
  run_id: "dr-100",
  question: QUESTION,
  terminal_state: "completed",
  report_md: "# Sodium-ion\n\nLower energy density, better cold performance.\n",
  claims: [],
  not_covered: [],
  residue: [],
  reframe: null,
  alignment: null,
  budget: { spent: { web: 8 }, remaining: { web: 0 } },
  rounds: [],
  consent: null,
  constitution: { passed_claims: 0, violations: [], unresolved: 0 },
};

/** The shelf as the backend reports it while dr-100 is being driven: no
 *  manifest yet, so `terminal_state` is absent — and `live` is what says
 *  the run is alive. Defaulting that absence to "interrupted" is the bug. */
const RUNS_WITH_LIVE: DrRunSummary[] = [
  {
    run_id: "dr-100",
    question: QUESTION,
    created_at_unix: 100,
    terminal_state: null,
    live: true,
    rounds: 0,
    report_present: false,
    consent: null,
  },
];

function stubDr(
  page: import("@playwright/test").Page,
  overrides: { runs?: DrRunSummary[]; activeRuns?: unknown[] } = {},
) {
  return page.evaluate(
    ([caps, runs, activeRuns]) => {
      const t = window.__sovereign_test__;
      t.setHandler("dr_capabilities", () => caps);
      t.setHandler("list_corpora", () => []);
      t.setHandler("dr_list_runs", () => runs);
      t.setHandler("dr_active_runs", () => activeRuns);
      t.setHandler("dr_start", (args: unknown) => {
        (t as unknown as { _lastDrStart: unknown })._lastDrStart = args;
        return { job_id: "job-1", channel: "deep-research://progress/job-1" };
      });
      t.setHandler("dr_abort", (args: unknown) => {
        (t as unknown as { _abortCalls: unknown[] })._abortCalls ??= [];
        (t as unknown as { _abortCalls: unknown[] })._abortCalls.push(args);
        return undefined;
      });
      t.setHandler("dr_open_report", () => null);
      t.setHandler("dr_quit_anyway", () => {
        (t as unknown as { _quitCalls: number })._quitCalls =
          ((t as unknown as { _quitCalls?: number })._quitCalls ?? 0) + 1;
        return undefined;
      });
      t.setHandler("notebook_list", () => []);
    },
    [CAPS, (overrides.runs ?? []) as DrRunSummary[], (overrides.activeRuns ?? []) as unknown[]] as const,
  );
}

function emitDr(page: import("@playwright/test").Page, event: DeepResearchRunProgress) {
  return page.evaluate(
    ([channel, payload]) => window.__sovereign_test__.emit(channel, payload),
    [CHANNEL, event] as const,
  );
}

/** A `live` snapshot with the fields the run view reads. */
function live(
  round: number | null,
  gaps: { id: string; text: string }[],
  spent = 2,
  remaining = 6,
): DeepResearchRunProgress {
  return {
    kind: "live",
    round,
    max_rounds: 3,
    stage: "rounding",
    gaps,
    budget: { spent: { web: spent }, remaining: { web: remaining } },
    consent: null,
  };
}

/** The backend's per-second tick. `quiet_secs` is how long the run dir has
 *  been unchanged — the number that separates "thinking" from "wedged". */
function beat(elapsed: number, quiet: number, stage = "rounding"): DeepResearchRunProgress {
  return { kind: "heartbeat", elapsed_secs: elapsed, quiet_secs: quiet, stage };
}

async function startRun(page: import("@playwright/test").Page) {
  await page.getByTestId("open-deep-research").click();
  await expect(page.getByTestId("deep-research-view")).toBeVisible();
  await page.getByTestId("dr-question").fill(QUESTION);
  await page.getByTestId("dr-start").click();
  await expect(page.getByTestId("dr-run-view")).toBeVisible();
}

test.describe("deep research — the run outlives the view", () => {
  test("leaving mid-run and coming back finds the run, not an empty composer", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);

    await emitDr(page, { kind: "started", run_id: "dr-100", run_dir: "/runs/dr-100" });
    await emitDr(page, live(2, [{ id: "g1", text: "cycle-life needs a second origin" }]));
    await emitDr(page, beat(120, 3));
    await expect(page.getByTestId("dr-round")).toContainText("round 2 of 3");

    // Away. This is the click that used to throw the run away.
    await page.getByTestId("dr-back").click();
    await expect(page.locator(".chat-view")).toBeVisible();

    // The run advances while the operator is somewhere else entirely.
    await emitDr(page, live(3, [{ id: "g2", text: "cost per kWh is uncorroborated" }], 6, 2));
    await emitDr(page, beat(240, 5));

    // Back.
    await page.getByTestId("open-deep-research").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
    await expect(page.getByTestId("dr-composer")).toHaveCount(0);
    // Everything that happened while they were away is here.
    await expect(page.getByTestId("dr-round")).toContainText("round 3 of 3");
    await expect(page.getByTestId("dr-gap-g2")).toContainText("cost per kWh");
    await expect(page.getByTestId("dr-elapsed")).toHaveText("4m 00s");
    await expect(page.getByTestId("dr-meter-web")).toContainText("6 spent");
    // And the trail keeps BOTH rounds — leaving did not rewrite history to
    // look like the run only just got here.
    await expect(page.getByTestId("dr-trail-2")).toContainText("second origin");
    await expect(page.getByTestId("dr-trail-3")).toContainText("cost per kWh");
  });

  test("the run is visible from anywhere in the app, and clicking it returns", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));
    await emitDr(page, beat(65, 2));

    // On the deep-research surface the pill is suppressed — the run is
    // already fully rendered there.
    await expect(page.getByTestId("dr-presence")).toHaveCount(0);

    await page.getByTestId("dr-back").click();
    await expect(page.locator(".chat-view")).toBeVisible();

    const pill = page.getByTestId("dr-presence");
    await expect(pill).toBeVisible();
    await expect(page.getByTestId("dr-presence-label")).toHaveText("Researching");
    await expect(page.getByTestId("dr-presence-detail")).toContainText("round 2 of 3");
    await expect(page.getByTestId("dr-presence-detail")).toContainText("1m 05s");

    await pill.click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
    await expect(page.getByTestId("dr-round")).toContainText("round 2 of 3");
  });

  test("a report that lands while the operator is elsewhere is announced, not lost", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(3, []));
    await page.getByTestId("dr-back").click();
    await expect(page.locator(".chat-view")).toBeVisible();

    // It finishes with nobody watching. This event used to hit a torn-down
    // listener and the completed research was never surfaced at all.
    await emitDr(page, { kind: "report_ready", report: REPORT });

    const notice = page.getByTestId("dr-presence-finished");
    await expect(notice).toBeVisible();
    await expect(notice).toContainText("Research finished");
    await expect(notice).toContainText(QUESTION);

    await page.getByTestId("dr-presence-open").click();
    await expect(page.getByTestId("dr-report-view")).toBeVisible();
    await expect(page.getByTestId("dr-report-question")).toHaveText(QUESTION);
    // Acknowledged by being read: the notice retires itself.
    await expect(page.getByTestId("dr-presence-finished")).toHaveCount(0);
  });

  test("a failure that lands while the operator is elsewhere is announced too", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await page.getByTestId("dr-back").click();
    await expect(page.locator(".chat-view")).toBeVisible();

    await emitDr(page, { kind: "failed", error: "the search provider refused the release" });
    await expect(page.getByTestId("dr-presence-finished")).toContainText("Research failed");
    await page.getByTestId("dr-presence-open").click();
    await expect(page.getByTestId("dr-run-failed")).toContainText("refused the release");
  });

  test("the report is shown immediately when the operator is watching", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "report_ready", report: REPORT });
    await expect(page.getByTestId("dr-report-view")).toBeVisible();
    // No floating notice on top of the thing it is announcing.
    await expect(page.getByTestId("dr-presence-finished")).toHaveCount(0);
  });

  test("a run already in flight is adopted, even if this view never saw it start", async ({
    sovereignPage: page,
    chat,
  }) => {
    // The backend survives a webview reload; the store asks it what is
    // running rather than assuming nothing is. The handler goes in via
    // addInitScript, not setHandler, because it has to be answering BEFORE
    // the app mounts — which is the whole scenario.
    await page.addInitScript(
      ([channel, question, runs]) => {
        const install = () => {
          const t = window.__sovereign_test__;
          if (!t) return false;
          t.setHandler("dr_active_runs", () => [
            {
              run_id: "dr-100",
              channel,
              question,
              started_at_unix: 1_700_000_000,
            },
          ]);
          t.setHandler("dr_list_runs", () => runs);
          return true;
        };
        if (!install()) queueMicrotask(install);
      },
      [CHANNEL, QUESTION, RUNS_WITH_LIVE] as const,
    );
    await bootToChat(page, chat);
    await stubDr(page, { runs: RUNS_WITH_LIVE });

    // The pill finds it without anyone opening deep research first.
    await expect(page.getByTestId("dr-presence")).toBeVisible();
    await emitDr(page, live(1, [{ id: "g1", text: "an adopted gap" }]));
    await emitDr(page, beat(30, 1));

    await page.getByTestId("dr-presence").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
    await expect(page.getByTestId("dr-gap-g1")).toContainText("an adopted gap");
    await expect(page.getByTestId("dr-elapsed")).toHaveText("30s");
  });
});

test.describe("deep research — is it working, and how", () => {
  test("elapsed advances with the backend's heartbeat", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);

    // Before the first beat there is no honest elapsed to show, so none is
    // invented.
    await expect(page.getByTestId("dr-elapsed")).toHaveText("—");
    await emitDr(page, beat(9, 1));
    await expect(page.getByTestId("dr-elapsed")).toHaveText("9s");
    await emitDr(page, beat(3610, 4));
    await expect(page.getByTestId("dr-elapsed")).toHaveText("1h 00m");
  });

  test("a long silence inside a round is named as normal, not left ambiguous", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));

    // Short quiet: unremarkable, and saying more would be noise.
    await emitDr(page, beat(70, 5));
    await expect(page.getByTestId("dr-liveness")).toHaveAttribute("data-state", "working");

    // Long quiet, still beating: the run is fine and the surface says WHY
    // nothing is moving, instead of showing a frozen panel.
    await emitDr(page, beat(200, 130));
    await expect(page.getByTestId("dr-liveness")).toHaveAttribute("data-state", "quiet");
    await expect(page.getByTestId("dr-liveness")).toContainText("2m 10s");
    await expect(page.getByTestId("dr-liveness")).toContainText("normal");
  });

  test("a backend that stops ticking is reported as no signal, not as working", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));
    await emitDr(page, beat(60, 2));
    await expect(page.getByTestId("dr-liveness")).toHaveAttribute("data-state", "working");

    // Nothing further is emitted. The run dir stopped changing AND the
    // backend stopped talking — which is a different fact from "the round
    // is thinking", and the one a two-state indicator cannot express.
    await expect
      .poll(
        () => page.getByTestId("dr-liveness").getAttribute("data-state"),
        { timeout: 15_000, intervals: [500] },
      )
      .toBe("no-signal");
    await expect(page.getByTestId("dr-liveness")).toContainText("No signal");
    // And the elapsed clock does NOT keep counting confidently past the
    // last thing the backend actually told us.
    await expect(page.getByTestId("dr-elapsed")).toHaveText("1m 00s");
  });

  test("the pill reports lost signal too, from wherever the operator is", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, beat(60, 2));
    await page.getByTestId("dr-back").click();
    await expect(page.getByTestId("dr-presence-label")).toHaveText("Researching");

    await expect
      .poll(
        () => page.getByTestId("dr-presence").getAttribute("data-liveness"),
        { timeout: 15_000, intervals: [500] },
      )
      .toBe("no-signal");
    await expect(page.getByTestId("dr-presence-label")).toContainText("No signal");
  });

  test("an unknown stage is shown verbatim rather than flattened", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { ...live(1, []), stage: "reconciling" });
    // A label we cannot explain is still information. Substituting a
    // generic "working" would be the silent substitution this surface
    // exists to stop making.
    await expect(page.getByTestId("dr-stage")).toHaveText("reconciling");
    await emitDr(page, { ...live(1, []), stage: "checking" });
    await expect(page.getByTestId("dr-stage")).toHaveText(
      "Checking the draft against its evidence",
    );
  });

  test("the run states plainly that leaving does not stop it", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await expect(page.getByTestId("dr-detach-note")).toContainText("keeps going if you leave");
  });
});

test.describe("deep research — a live run is never mistaken for a dead one", () => {
  test("the shelf calls a running run running, offers Watch, and never offers Resume", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page, { runs: RUNS_WITH_LIVE });
    await page.getByTestId("open-deep-research").click();
    await expect(page.getByTestId("deep-research-view")).toBeVisible();

    // It used to read `interrupted` — the manifest's absence defaulted into
    // a terminal state — with a Resume button that would have handed a
    // second loop the run dir the first was mid-write on.
    await expect(page.getByTestId("dr-run-state-dr-100")).toHaveText("running");
    await expect(page.getByTestId("dr-resume-dr-100")).toHaveCount(0);
    await expect(page.getByTestId("dr-watch-dr-100")).toBeVisible();
  });

  test("a run with no manifest that nobody is driving is still interrupted", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page, {
      runs: [{ ...RUNS_WITH_LIVE[0], run_id: "dr-99", live: false }],
    });
    await page.getByTestId("open-deep-research").click();
    // `live` outranks the manifest; absent-and-not-live is genuinely
    // interrupted, and that run IS resumable.
    await expect(page.getByTestId("dr-run-state-dr-99")).toHaveText("interrupted");
    await expect(page.getByTestId("dr-resume-dr-99")).toBeVisible();
  });

  test("reaching the composer with a run in flight says so and offers the way back", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));
    await emitDr(page, beat(90, 3));

    await page.getByTestId("dr-run-to-ask").click();
    await expect(page.getByTestId("dr-composer")).toBeVisible();
    const notice = page.getByTestId("dr-running-notice");
    await expect(notice).toContainText("round 2 of 3");
    await expect(notice).toContainText("1m 30s");
    await expect(notice).toContainText("One runs at a time");

    await page.getByTestId("dr-back-to-run").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
  });
});

test.describe("deep research — the run ends when the operator says so", () => {
  test("stopping is confirmed, and the confirmation can be declined", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, beat(600, 4));

    await page.getByTestId("dr-abort").click();
    const confirm = page.getByTestId("dr-stop-confirm");
    await expect(confirm).toBeVisible();
    // The honest promise: not a kill, a truncated report with the
    // truncation declared.
    await expect(confirm).toContainText("keep what it has gathered");
    await expect(confirm).toContainText("early stop declared");

    // Declining leaves the run entirely alone.
    await page.getByTestId("dr-stop-cancel").click();
    await expect(confirm).toHaveCount(0);
    const calls = await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _abortCalls?: unknown[] })._abortCalls ?? [],
    );
    expect(calls).toHaveLength(0);
    await expect(page.getByTestId("dr-abort")).toBeEnabled();
  });

  test("confirming stops the run and reports that it is finishing, not that it died", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "started", run_id: "dr-100", run_dir: "/runs/dr-100" });
    await emitDr(page, beat(600, 4));

    await page.getByTestId("dr-abort").click();
    await page.getByTestId("dr-stop-confirm-yes").click();

    const calls = (await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _abortCalls?: unknown[] })._abortCalls ?? [],
    )) as { jobId: string }[];
    expect(calls).toHaveLength(1);
    expect(calls[0].jobId).toBe("job-1");

    // The run has not ended yet — the backend is writing the report. The
    // button says exactly that rather than going dead or claiming success.
    await expect(page.getByTestId("dr-abort")).toContainText("Stopping");
    await expect(page.getByTestId("dr-abort")).toBeDisabled();
  });

  test("a stop the operator asked for is reported as a stop, not a failure", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, beat(600, 4));
    await page.getByTestId("dr-abort").click();
    await page.getByTestId("dr-stop-confirm-yes").click();
    await page.getByTestId("dr-back").click();

    // The truncated report the backend lands is the outcome the operator
    // ASKED for. Calling it a failure would be a lie about their own
    // instruction.
    await emitDr(page, { kind: "report_ready", report: { ...REPORT, terminal_state: "truncated" } });
    await expect(page.getByTestId("dr-presence-finished")).toContainText(
      "Research stopped — findings kept",
    );
  });

  test("the stop affordance is not offered for a run that already ended", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "failed", error: "boom" });
    await expect(page.getByTestId("dr-run-failed")).toContainText("boom");
    await expect(page.getByTestId("dr-abort")).toBeDisabled();
  });
});

test.describe("deep research — quitting mid-run is a decision, not an accident", () => {
  /** The window's `CloseRequested` handler refuses the close and emits this
   *  when research is in flight; the frontend owns the conversation. */
  const emitQuitBlocked = (page: import("@playwright/test").Page) =>
    page.evaluate(() =>
      window.__sovereign_test__.emit("deep-research://quit-blocked", null),
    );

  test("closing the app mid-run asks first, and staying is the default-weighted choice", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));
    await emitDr(page, beat(500, 3));

    await emitQuitBlocked(page);
    const dialog = page.getByTestId("dr-quit-blocked");
    await expect(dialog).toBeVisible();
    // It names WHAT is running, not just that something is.
    await expect(dialog).toContainText(QUESTION);
    await expect(dialog).toContainText("round 2 of 3");
    await expect(dialog).toContainText("8m 20s");
    // And what closing actually costs — the run dir survives, so the honest
    // consequence is a resumable run, not a lost one.
    await expect(dialog).toContainText("resume the run next time");

    await page.getByTestId("dr-quit-stay").click();
    await expect(dialog).toHaveCount(0);
    const quits = await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _quitCalls?: number })._quitCalls ?? 0,
    );
    expect(quits).toBe(0);
    // The run was never touched.
    await expect(page.getByTestId("dr-round")).toContainText("round 2 of 3");
  });

  test("choosing to close anyway goes through", async ({ sovereignPage: page, chat }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, beat(500, 3));
    await emitQuitBlocked(page);

    await page.getByTestId("dr-quit-anyway").click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window.__sovereign_test__ as unknown as { _quitCalls?: number })._quitCalls ?? 0,
        ),
      )
      .toBe(1);
    await expect(page.getByTestId("dr-quit-blocked")).toHaveCount(0);
  });

  test("the quit guard is answerable from the deep-research surface too", async ({
    sovereignPage: page,
    chat,
  }) => {
    // `hidden` suppresses the pill on this surface; it must NOT suppress a
    // refusal to close the whole app.
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, beat(500, 3));
    await expect(page.getByTestId("dr-presence")).toHaveCount(0);

    await emitQuitBlocked(page);
    await expect(page.getByTestId("dr-quit-blocked")).toBeVisible();
  });
});

test.describe("deep research — one run at a time", () => {
  test("the composer will not start a second run over a live one", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, live(2, []));
    await emitDr(page, beat(90, 3));

    await page.getByTestId("dr-run-to-ask").click();
    await expect(page.getByTestId("dr-composer")).toBeVisible();
    // The desktop can represent exactly one run, so a second start would
    // leave the first with no surface and no listener — the very failure
    // this change set removes. The backend refuses it too; this stops the
    // operator walking into that refusal.
    await expect(page.getByTestId("dr-start")).toBeDisabled();
    await expect(page.getByTestId("dr-start")).toHaveText("A run is already going");
    await expect(page.getByTestId("dr-running-notice")).toContainText("One runs at a time");
  });

  test("the composer starts runs again once the live one ends", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "failed", error: "boom" });
    await page.getByTestId("dr-back").click();
    await page.getByTestId("open-deep-research").click();
    await page.getByTestId("dr-question").fill("A second question");
    await expect(page.getByTestId("dr-start")).toBeEnabled();
    await page.getByTestId("dr-start").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
  });
});

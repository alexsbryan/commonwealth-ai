// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Deep research — scene 1 (order deep-research-t3b). The desktop is a
// DRIVER over the CLI verb's contract: the Ask entry forwards the question +
// budget + typed consent grant (default-deny) as verb flags, the live view
// renders ONLY the run-dir artifacts the verb wrote (round, the gate's named
// gap list, the budget ledger, the consent-grant status), and the report view
// renders the verb's own checked report with its verdict dimensions
// (corroboration / residue / reframe) and the constitution position.
//
// The Tauri runtime is mocked (fixtures/tauri-shim.js): `dr_start` returns a
// handle whose channel the spec drives with `emit`, so these tests pin the
// UI's contract against the event surface — the same events the Rust driver
// emits from the verb's artifacts.

import type { DeepResearchRunProgress, DrReport, DrRunSummary } from "../../src/lib/types";

const CAPS = {
  cli_path: "/usr/local/bin/sovereign",
  flags: ["--run-dir", "--max-rounds", "--corpora", "--consent", "--resume"],
  error: null,
};

const CAPS_NO_RESUME = {
  cli_path: "/usr/local/bin/sovereign",
  flags: ["--run-dir", "--max-rounds", "--corpora", "--consent"],
  error: null,
};

const REPORT: DrReport = {
  run_id: "dr-100",
  question: "When did Apollo 11 land on the Moon?",
  terminal_state: "completed",
  report_md: "# Apollo 11\n\nThe mission landed on July 20, 1969.\n",
  claims: [
    {
      id: "c1",
      text: "Apollo 11 landed on July 20, 1969.",
      verdict: "passed",
      status: "passed",
      citations: [{ evidence_id: "ev-1", url: "https://example.com/a", chunk_id: "ev-1" }],
      corroboration: {
        origins: ["https://example.com/a", "https://example.com/b"],
        support_chunks: 2,
        floor: 2,
        passes_floor: true,
      },
    },
  ],
  not_covered: [],
  residue: [{ query: "Apollo 11 landing crew 1969 transcript", round: 2 }],
  reframe: null,
  alignment: null,
  budget: { spent: { web: 4 }, remaining: { web: 4 } },
  rounds: [{ round: 1, gaps_before: 3, gaps_after: 1, fetched: 4, search_calls: 2 }],
  consent: { release_floor: "public-web", granted_at_unix: 100 },
  constitution: { passed_claims: 1, violations: [], unresolved: 0 },
};

const RUNS: DrRunSummary[] = [
  {
    run_id: "dr-100",
    question: "When did Apollo 11 land on the Moon?",
    created_at_unix: 100,
    terminal_state: "completed",
    live: false,
    rounds: 1,
    report_present: true,
    consent: { release_floor: "public-web", granted_at_unix: 100 },
  },
  {
    run_id: "dr-99",
    question: "Interrupted run",
    created_at_unix: 99,
    // No manifest and nobody driving it: genuinely interrupted, and the
    // label is derived from the pair rather than defaulted into this field.
    terminal_state: null,
    live: false,
    rounds: 0,
    report_present: false,
    consent: null,
  },
];

function stubDr(page: import("@playwright/test").Page, overrides: Record<string, unknown> = {}) {
  return page.evaluate(
    ([caps, corpora, runs, ov]) => {
      const t = window.__sovereign_test__;
      t.setHandler("dr_capabilities", () => caps);
      t.setHandler("list_corpora", () => corpora);
      t.setHandler("dr_list_runs", () => runs);
      t.setHandler("dr_active_runs", () => []);
      t.setHandler("dr_start", (args: unknown) => {
        (t as unknown as { _lastDrStart: unknown })._lastDrStart = args;
        return { job_id: "job-1", channel: "deep-research://progress/job-1" };
      });
      t.setHandler("dr_abort", () => undefined);
      t.setHandler("dr_open_report", () => ov.report ?? null);
      t.setHandler("notebook_list", () => []);
    },
    [
      (overrides.caps ?? CAPS) as typeof CAPS,
      (overrides.corpora ?? [
        { id: "sep", name: "SEP", status: "installed" },
        { id: "estate-dr-100", name: "estate-dr-100", status: "installed" },
      ]) as { id: string; name: string; status: string }[],
      (overrides.runs ?? RUNS) as DrRunSummary[],
      overrides,
    ] as const,
  );
}

async function startRun(page: import("@playwright/test").Page) {
  await page.getByTestId("open-deep-research").click();
  await expect(page.getByTestId("deep-research-view")).toBeVisible();
  await page.getByTestId("dr-question").fill("When did Apollo 11 land on the Moon?");
  await page.getByTestId("dr-start").click();
  await expect(page.getByTestId("dr-run-view")).toBeVisible();
}

function emitDr(page: import("@playwright/test").Page, event: DeepResearchRunProgress) {
  return page.evaluate(
    ([channel, payload]) => window.__sovereign_test__.emit(channel, payload),
    ["deep-research://progress/job-1", event] as const,
  );
}

test.describe("deep research — scene 1", () => {
  test("the Ask entry opens from the chat empty state, consent defaults to deny", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);

    await page.getByTestId("open-deep-research").click();
    await expect(page.getByTestId("deep-research-view")).toBeVisible();
    await expect(page.getByTestId("dr-composer")).toBeVisible();

    // Default-deny is the selected consent class, by default.
    await expect(page.getByTestId("dr-consent-deny")).toBeChecked();
    await expect(page.getByTestId("dr-caps-error")).toHaveCount(0);
  });

  test("a run with default-deny sends no consent flag; the live view renders the gate's artifacts", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);

    const sent = (await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _lastDrStart: unknown })._lastDrStart,
    )) as { question: string; options: Record<string, unknown> };
    expect(sent.question).toBe("When did Apollo 11 land on the Moon?");
    // Default-deny: no consent class leaves the Ask entry.
    expect(sent.options.consent).toBeNull();
    expect(sent.options.maxRounds).toBe(3);

    // The verb named its run dir.
    await emitDr(page, { kind: "started", run_id: "dr-100", run_dir: "/tmp/deep-research-runs/dr-100" });
    await expect(page.getByTestId("dr-run-id")).toHaveText("dr-100");

    // Round 1's named gaps + budget ledger + default-deny status.
    await emitDr(page, {
      kind: "live",
      round: 1,
      max_rounds: 3,
      stage: "rounding",
      gaps: [
        { id: "g1", text: "the landing date needs a second origin" },
        { id: "g2", text: "the crew manifest is uncorroborated" },
      ],
      budget: { spent: { web: 2 }, remaining: { web: 2 } },
      consent: null,
    });
    await expect(page.getByTestId("dr-stage")).toHaveAttribute("data-stage", "rounding");
    await expect(page.getByTestId("dr-round")).toContainText("round 1 of 3");
    await expect(page.getByTestId("dr-gap-g1")).toContainText("second origin");
    await expect(page.getByTestId("dr-gap-g2")).toContainText("crew manifest");
    await expect(page.getByTestId("dr-meter-web")).toContainText("2 spent");
    await expect(page.getByTestId("dr-meter-web")).toContainText("2 remaining");
    await expect(page.getByTestId("dr-consent-deny-live")).toBeVisible();
  });

  test("a typed consent grant rides the run and the live view shows it", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await page.getByTestId("open-deep-research").click();
    await page.getByTestId("dr-consent-public-web").check();
    await page.getByTestId("dr-question").fill("When did Apollo 11 land on the Moon?");
    await page.getByTestId("dr-start").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();

    const sent = (await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _lastDrStart: unknown })._lastDrStart,
    )) as { options: Record<string, unknown> };
    expect(sent.options.consent).toBe("public-web");

    await emitDr(page, {
      kind: "live",
      round: 0,
      max_rounds: 3,
      stage: "planning",
      gaps: [],
      budget: { spent: {}, remaining: { web: 4 } },
      consent: { release_floor: "public-web", granted_at_unix: 100 },
    });
    await expect(page.getByTestId("dr-consent-live")).toContainText("public-web");
    await expect(page.getByTestId("dr-consent-deny-live")).toHaveCount(0);
  });

  test("the report view renders the verb's checked report with its dimensions and constitution", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "report_ready", report: REPORT });

    await expect(page.getByTestId("dr-report-view")).toBeVisible();
    await expect(page.getByTestId("dr-report-question")).toHaveText(
      "When did Apollo 11 land on the Moon?",
    );
    // The report body is the verb's own report.md.
    await expect(page.getByTestId("dr-report-view")).toContainText("July 20, 1969");
    // Verdict dimensions: the gate's corroboration accounting on the claim.
    await expect(page.getByTestId("dr-claim-c1")).toContainText("passed");
    await expect(page.getByTestId("dr-corroboration-c1")).toContainText("2 chunks from 2 origins");
    await expect(page.getByTestId("dr-corroboration-c1")).toContainText("floor passed");
    // Residue: searched-but-absent is first-class report content.
    await expect(page.getByTestId("dr-residue")).toContainText("crew 1969 transcript");
    // Constitution: zero untraced figures in [passed].
    await expect(page.getByTestId("dr-constitution")).toContainText("Position holds");
  });

  test("a violated constitution is named, never defaulted", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, {
      kind: "report_ready",
      report: {
        ...REPORT,
        constitution: {
          passed_claims: 1,
          violations: ["claim c1 [passed] carries untraced figures: 2024"],
          unresolved: 1,
        },
      },
    });
    await expect(page.getByTestId("dr-constitution-violations")).toContainText("2024");
    await expect(page.getByTestId("dr-constitution-unresolved")).toContainText("1");
  });

  test("the completed run's estate corpus selects into the next ask", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "report_ready", report: REPORT });

    await page.getByTestId("dr-ask-again").click();
    await expect(page.getByTestId("dr-composer")).toBeVisible();
    // The estate corpus of THIS run (estate-dr-100) is preselected for the
    // next ask's --corpora; unrelated corpora stay off.
    await expect(page.getByTestId("dr-corpus-estate-dr-100")).toBeChecked();
    await expect(page.getByTestId("dr-corpus-sep")).not.toBeChecked();
  });

  test("the Library handoff is offered from the report view", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "report_ready", report: REPORT });
    await page.getByTestId("dr-open-library").click();
    await expect(page.getByTestId("library-view")).toBeVisible();
  });

  test("the previous-runs shelf lists runs; resume is gated on the verb's --resume flag", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page, { report: REPORT });
    await page.getByTestId("open-deep-research").click();
    await expect(page.getByTestId("deep-research-view")).toBeVisible();

    // With --resume in the verb's help: the interrupted run offers Resume.
    await expect(page.getByTestId("dr-run-dr-100")).toBeVisible();
    await expect(page.getByTestId("dr-run-dr-99")).toBeVisible();
    await expect(page.getByTestId("dr-resume-dr-99")).toBeVisible();
    // Completed runs open their report instead.
    await page.getByTestId("dr-open-dr-100").click();
    await expect(page.getByTestId("dr-report-view")).toBeVisible();
    await expect(page.getByTestId("dr-report-question")).toHaveText(
      "When did Apollo 11 land on the Moon?",
    );
  });

  test("without --resume in the verb's help the resume affordance is absent", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page, { caps: CAPS_NO_RESUME });
    await page.getByTestId("open-deep-research").click();
    await expect(page.getByTestId("deep-research-view")).toBeVisible();
    await expect(page.getByTestId("dr-run-dr-99")).toBeVisible();
    await expect(page.getByTestId("dr-resume-dr-99")).toHaveCount(0);
  });

  test("a failed run reports its cause and the Ask face starts another run", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    await emitDr(page, { kind: "failed", error: "deep-research exited 101 without a report" });
    await expect(page.getByTestId("dr-run-failed")).toContainText("exited 101");

    // A normal run after the failure still works (the FSM is not stuck).
    await page.getByTestId("dr-back").click();
    await page.getByTestId("open-deep-research").click();
    await page.getByTestId("dr-question").fill("Second question");
    await page.getByTestId("dr-start").click();
    await expect(page.getByTestId("dr-run-view")).toBeVisible();
    const sent = (await page.evaluate(
      () => (window.__sovereign_test__ as unknown as { _lastDrStart: unknown })._lastDrStart,
    )) as { question: string };
    expect(sent.question).toBe("Second question");
  });

  test("duplicate live events are idempotent — no throw, no stuck state", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await stubDr(page);
    await startRun(page);
    const live: DeepResearchRunProgress = {
      kind: "live",
      round: 1,
      max_rounds: 3,
      stage: "rounding",
      gaps: [{ id: "g1", text: "a gap" }],
      budget: { spent: { web: 1 }, remaining: { web: 3 } },
      consent: null,
    };
    await emitDr(page, live);
    await emitDr(page, live);
    await expect(page.getByTestId("dr-gap-g1")).toHaveCount(1);
    await expect(page.getByTestId("dr-stage")).toHaveAttribute("data-stage", "rounding");
    // No uncaught exceptions (auto-enforced by the fixture), and the abort
    // affordance is still live.
    await expect(page.getByTestId("dr-abort")).toBeVisible();
  });
});

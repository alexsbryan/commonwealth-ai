// SPDX-License-Identifier: AGPL-3.0-or-later
// Prompt-budget guard regression (Phase 1 of the budget-sensor
// redesign; note 2cd9227e's deeper class). On this suite's 8192-token
// profile, a few large pasted messages used to push assembly past the
// context window and every subsequent turn died with the engine's
// terminal "Prompt too long … Shorten the conversation" error. The
// guard must instead trim (history → evidence → response reservation),
// keep every turn terminating in message-complete, and glassbox the
// degradation via metadata.prompt_budget.
import { expect, test } from "../test-base-real";
import { eventsRecent } from "./spawn";

const BRIDGE = "http://127.0.0.1:9745";

test("long thread on a small window: turns keep completing, trims are glassboxed", async ({
  sovereignPage: page,
  bridge,
}) => {
  test.setTimeout(600_000);
  await page.goto("/");
  await page.locator(".chat-view").waitFor({ state: "visible", timeout: 30_000 });
  await page.locator(".new-btn").click();

  // Big pasted user messages fill the window fast without paying for
  // long generations. ~5k chars each ≈ 1.2k tokens of history apiece
  // (pre-truncation) plus retrieval + system on every turn.
  const filler = "The tide ledger records storm glass readings and barometric drift. ".repeat(70);
  const turns = [
    `Summarize this in one short sentence: ${filler}`,
    `And this too, one short sentence: ${filler}`,
    `One more, single sentence: ${filler}`,
    "Given everything above, when was the Meridian Lighthouse automated?",
  ];

  let sawBudgetNote = false;
  for (const [i, message] of turns.entries()) {
    const since = (await eventsRecent(BRIDGE)).at(-1)?.seq ?? 0;
    await page.locator(".input-area textarea").fill(message);
    await page.locator(".send-btn").click();

    // Every turn must reach message-complete — never the engine's
    // prompt-too-long error, never a hang.
    await expect
      .poll(
        async () => {
          const rows = await eventsRecent(BRIDGE, since);
          const err = rows.find((r) => r.event === "message-error");
          if (err) {
            const msg = JSON.stringify(err.payload);
            if (/prompt too long/i.test(msg)) return `PROMPT_TOO_LONG: ${msg}`;
            return `error: ${msg}`;
          }
          return rows.some((r) => r.event === "message-complete") ? "complete" : "waiting";
        },
        { timeout: 150_000, intervals: [1000, 2000] },
      )
      .toBe("complete");

    const rows = await eventsRecent(BRIDGE, since);
    const complete = rows.filter((r) => r.event === "message-complete").pop();
    const meta = (complete?.payload as { metadata?: Record<string, unknown> })?.metadata;
    if (meta && typeof meta.prompt_budget === "string" && meta.prompt_budget.length > 0) {
      sawBudgetNote = true;
      console.log(`  turn ${i + 1} budget note: ${meta.prompt_budget}`);
    }
    await page.locator(".send-btn").waitFor({ state: "visible", timeout: 30_000 });
  }

  // On an 8192 window this thread MUST have engaged the budget
  // machinery somewhere: either the Phase-1 trim ladder (note in
  // metadata) or — once Phases 2+3 converge the sensor and scale
  // assembly proactively — the memo-driven ceilings/allocation,
  // whose evidence is the runtime:prompt_budget / ctx-aware traces
  // in the app log. If NEITHER fired, the window grew (update the
  // profile) or the machinery regressed to a no-op.
  const fs = await import("node:fs");
  const path = await import("node:path");
  const { fileURLToPath } = await import("node:url");
  const here = path.dirname(fileURLToPath(import.meta.url));
  const appLog = fs.readFileSync(
    path.resolve(here, "../../../../test-artifacts/faults-app.log"),
    "utf8",
  );
  const proactive =
    /ctx-aware retrieval budget tighter|allocation: scaling history|raising estimate to last turn's measured demand|prompt budget enforced/.test(
      appLog,
    );
  expect(
    sawBudgetNote || proactive,
    "neither metadata.prompt_budget nor any budget-machinery trace fired on an 8k window",
  ).toBe(true);
  console.log(
    `  budget machinery evidence: trim-note=${sawBudgetNote} proactive-traces=${proactive}`,
  );

  // And the conversation is still usable — the original bricking
  // class left it permanently dead.
  const convs = await bridge.invoke<Array<{ id: string }>>("list_conversations");
  expect(convs.length).toBeGreaterThan(0);
});

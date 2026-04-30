import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { test, expect, bootToChat } from "../fixtures/test-base";
import {
  playScenario,
  type Scenario,
  type TtfiReport,
} from "../fixtures/scenario-player";
import { fastLocal } from "../scenarios/fast-local";
import { knowledgeGrounded } from "../scenarios/knowledge-grounded";
import { heavyReasoning } from "../scenarios/heavy-reasoning";
import { disambiguation } from "../scenarios/disambiguation";
import { offTargetSuppressed } from "../scenarios/off-target-suppressed";

// Time-to-First-Intelligence harness. Each scenario replays a
// representative backend timing shape; the in-page probe records when
// the user first sees each tier of "intelligence signal" (generic
// loading / specific stage label / aux signal / first content). Output
// is a JSON report consumed by `npm run report:ttfi`.
//
// Soft assertion mode: budget overruns are console.warn'd but never
// fail a test. The metric is still being characterized — flaky reds
// while we tune would burn the harness's credibility. Promote stable
// scenarios to hard `expect()` once the numbers settle.

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPORT_PATH = path.resolve(__dirname, "../.ttfi-report.json");

type ReportRow = {
  scenario: string;
  description: string;
  ttfi: TtfiReport;
  budgets?: Scenario["budgets"];
  warnings: string[];
};

const rows: ReportRow[] = [];

const scenarios: Scenario[] = [
  fastLocal,
  knowledgeGrounded,
  heavyReasoning,
  disambiguation,
  offTargetSuppressed,
];

test.describe.configure({ mode: "serial" });

test.describe("Time to First Intelligence", () => {
  test.afterAll(async () => {
    const out = {
      generated_at: new Date().toISOString(),
      rows,
    };
    fs.writeFileSync(REPORT_PATH, JSON.stringify(out, null, 2) + "\n", "utf8");
    // eslint-disable-next-line no-console
    console.log(
      `\n[ttfi] wrote ${rows.length} rows to ${path.relative(process.cwd(), REPORT_PATH)}`,
    );
  });

  for (const scenario of scenarios) {
    test(`TTFI · ${scenario.name}`, async ({ sovereignPage: page, chat }) => {
      await bootToChat(page, chat);

      // Fill query, then anchor t0 IMMEDIATELY before the click so the
      // probe captures the click moment as scenario time zero.
      await page.locator(".input-area textarea").fill(scenario.query);
      await chat.api.ttfi.markStart();
      await page.locator(".send-btn").click();

      // Wait for the shim to record the streaming id assigned by the
      // mocked send_message_stream invoke. Without this, message-chunk
      // events would target an undefined id.
      await expect
        .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
        .not.toBeNull();
      const start = (await chat.api.lastStreamStart())!;

      await playScenario(
        page,
        { conversationId: start.conversationId, messageId: start.messageId },
        scenario,
      );

      // Wait for the scenario's declared terminal state. Two flavours:
      //   • send-btn-visible — the FSM returned to idle (chunks
      //     completed, error fired, or doc-asset path resolved).
      //   • selector-visible — a static affordance landed (e.g.,
      //     ClarificationCard) and the scenario ends without a
      //     completion event.
      if (scenario.terminal.kind === "send-btn-visible") {
        await expect(page.locator(".send-btn")).toBeVisible({
          timeout: 15_000,
        });
      } else {
        await expect(page.locator(scenario.terminal.selector)).toBeVisible({
          timeout: 15_000,
        });
      }

      const ttfi = await chat.api.ttfi.getReport();

      const warnings: string[] = [];
      if (scenario.budgets) {
        for (const [k, budget] of Object.entries(scenario.budgets)) {
          if (budget == null) continue;
          const value = ttfi[k as keyof TtfiReport];
          if (value != null && value > budget) {
            const msg = `${k}=${value.toFixed(0)}ms exceeds budget ${budget}ms`;
            warnings.push(msg);
            // eslint-disable-next-line no-console
            console.warn(`[ttfi.${scenario.name}] ${msg} (advisory)`);
          }
        }
      }

      rows.push({
        scenario: scenario.name,
        description: scenario.description,
        ttfi,
        budgets: scenario.budgets,
        warnings,
      });

      // The harness must have observed at least the generic tier —
      // otherwise the probe never wired up or the page never rendered
      // a loading state, both of which mean the run is invalid. This
      // is the only hard assertion: TTFI must be measurable.
      expect(ttfi.generic).not.toBeNull();
    });
  }
});

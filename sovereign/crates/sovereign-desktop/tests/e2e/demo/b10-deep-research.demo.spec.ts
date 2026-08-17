// SPDX-License-Identifier: AGPL-3.0-or-later
// B10 — deep research, scene 1 (order deep-research-t3b).
//
// The desktop is a DRIVER over the CLI verb's contract: the Ask entry
// forwards a question + budget + typed consent grant (default-deny), the
// live view renders ONLY the run-dir artifacts the verb wrote (round,
// the gate's named gaps, the budget ledger, the consent-grant status),
// and the report view renders the verb's own checked report with its
// verdict dimensions and the constitution position.
//
// Determinism: the run is served from the bank v1 report-class deck —
// `SOVEREIGN_DEMO_DR_FLAGS` (set in demo global-setup, declared in
// quality/env-flags.toml) appends `--backend mock --mock-deck <deck>` to
// the verb's args, so search/fetch resolve against the deck while drafts
// still go through the real daemon. The deck is the single source for
// the evidence; the model's turns vary, but the constitution position
// (zero untraced figures in [passed]) is checked against that evidence
// by the shared decider, so the (g) property is asserted for real.
//
// Consent honesty: the beat types a public-web grant to film the typed
// release UX; the grant is recorded in the run's charter. The deck
// serves every hit, so nothing actually leaves the machine — the caption
// says exactly that.
import { beatTest, expect, demoClick, demoType } from "./beat";
import { realBootToChat } from "./demo-base";
import { hasCorpus } from "./preflight";

const QUESTION =
  "How did American cities change across four decades (1980–2024): " +
  "gentrification, inequality, affordability, and displacement — every claim cited?";

beatTest(
  {
    id: "b10-deep-research",
    title: "Deep research: a question, a budget, a checked report",
    claim:
      "Ask with a budget and a typed release; watch the rounds, the gate's " +
      "named gaps, and the budget ledger live; read the checked report with " +
      "its verdicts — every [passed] figure traced in the evidence.",
    gifPadSec: 1.0,
    gifMark: "report-ready",
  },
  async ({ page, run }) => {
    run.requireOrSkip(
      await hasCorpus("sep"),
      "the `sep` corpus is not hosted by the daemon on :9741 — ingest it before capturing B10",
    );

    await realBootToChat(page);
    await run.dwell(1200);
    run.mark("open");

    await demoClick(page, page.getByTestId("open-deep-research"), { settleMs: 600 });
    await expect(page.getByTestId("deep-research-view")).toBeVisible();
    run.mark("ask-face");
    await run.dwell(1600);

    // ── The Ask entry: question + budget + typed consent (default-deny). ──
    await expect(page.getByTestId("dr-composer")).toBeVisible();
    await expect(page.getByTestId("dr-consent-deny")).toBeChecked();
    await run.caption("Default-deny is the standing posture — no release leaves the estate.", 3000);

    await demoType(page, page.getByTestId("dr-question"), QUESTION, { charDelayMs: 14 });
    await run.dwell(600);

    // Budget: two rounds, a modest acquisition budget per round.
    // fill() replaces the value wholesale — a select-all + type dance is
    // fragile here because demoType's focusFirst click collapses the
    // selection (observed: "3" became "32" mid-run, charter max_rounds 32).
    await demoClick(page, page.getByTestId("dr-rounds"), { settleMs: 300 });
    await page.getByTestId("dr-rounds").fill("2");
    await demoClick(page, page.getByTestId("dr-search"), { settleMs: 300 });
    await page.getByTestId("dr-search").fill("3");
    await demoClick(page, page.getByTestId("dr-fetch"), { settleMs: 300 });
    await page.getByTestId("dr-fetch").fill("3");

    // The estate-first affordance films itself: the corpus chips row
    // renders the operator's shelf ("consult these corpora first"). No
    // chip is clicked — an unscoped run keeps the estate leg empty and
    // the deck as the single evidence source (deterministic), which the
    // caption states honestly.
    const sepChip = page.getByTestId("dr-corpus-sep");
    if (await sepChip.isVisible().catch(() => false)) {
      run.note("estate shelf rendered on the Ask entry (sep among the chips); run left unscoped");
    } else {
      run.note("no corpus chips on this shelf — deck serves the evidence either way");
    }

    await run.caption(
      "A typed release: public-web, recorded in the run's charter. " +
        "The deck serves this run's evidence — nothing leaves the machine.",
      4200,
    );
    await demoClick(page, page.getByTestId("dr-consent-public-web"), { settleMs: 400 });
    await expect(page.getByTestId("dr-consent-public-web")).toBeChecked();
    run.mark("consent-typed");

    await demoClick(page, page.getByTestId("dr-start"), { settleMs: 700 });
    await expect(page.getByTestId("dr-run-view")).toBeVisible({ timeout: 60_000 });
    run.mark("run-live");
    await run.dwell(2400);

    // The live view, fed by the run-dir the verb writes: stage + round +
    // run id appear as soon as the driver's poll reads them.
    await expect(page.getByTestId("dr-run-id")).toContainText(/^dr-/, { timeout: 60_000 });
    await expect(page.getByTestId("dr-stage")).toContainText(/\S/);
    run.note(`run ${(await page.getByTestId("dr-run-id").textContent())?.trim()} live`);

    await run.caption(
      "Live from the run dir: the gate's named gaps, the budget ledger, " +
        "and the consent-grant status.",
      3600,
    );
    // Best-effort film of the gate's gaps between rounds — they render as
    // soon as the verb closes a round, so their presence is timing-bound.
    const gaps = page.getByTestId("dr-gaps");
    const gapsWatch = gaps
      .waitFor({ state: "visible", timeout: 150_000 })
      .then(async () => {
        if ((await gaps.locator("li").count()) > 0) {
          run.mark("gaps-live");
          await run.dwell(1800);
        }
      })
      .catch(() => run.note("rounds closed too fast to film the gap list — run-dir state asserted at the end"));

    const meters = page.getByTestId("dr-meters");
    await expect(meters.first().or(page.getByText("No spend yet.")).first()).toBeVisible();
    if ((await meters.locator("li").count()) > 0) {
      await expect(meters.locator("li").first()).toContainText(/\d+ spent/);
      run.mark("meter-live");
    }

    // The typed grant's status is rendered from the run's charter.
    const consentLive = page.getByTestId("dr-consent-live");
    await consentLive.waitFor({ state: "visible", timeout: 120_000 }).catch(() =>
      run.note(
        "consent-grant status not rendered before report close (should have been, from the charter)",
      ),
    );
    if (await consentLive.isVisible().catch(() => false)) {
      await expect(consentLive).toContainText("public-web");
      run.mark("consent-live");
    }

    // ── The report: the verb's own checked artifact. ──
    // The run is real: plan + up to two rounds of deck acquisition, each
    // with a draft and a gate pass on the primary. That is minutes, not
    // seconds — the cap is generous and the wait is honest.
    await expect(page.getByTestId("dr-report-view")).toBeVisible({ timeout: 900_000 });
    run.mark("report-ready");
    await gapsWatch;

    await expect(page.getByTestId("dr-report-question")).toContainText("four decades");
    const body = page.getByTestId("dr-report-view").locator(".dr-report-body");
    await expect(body).toContainText(/\S/);
    run.note(
      `report: ${(await page.getByTestId("dr-report-question").textContent())?.trim().slice(0, 80)}`,
    );
    // The budget must have reached the verb: the Ask UI clamps rounds to
    // 1..6, so a report claiming more is a mis-wired budget (observed
    // once as charter max_rounds 32 from an append-typed number input).
    const reportHead = (await page
      .getByTestId("dr-report-view")
      .locator(".dr-report-head .dr-muted")
      .textContent()) ?? "";
    const rounds = Number(reportHead.match(/· (\d+) rounds/)?.[1] ?? -1);
    expect(rounds, `report head must state the round count, got "${reportHead}"`).toBeGreaterThanOrEqual(1);
    expect(
      rounds,
      `the run must honor the Ask budget (rounds clamped to 1..6), report said ${rounds}`,
    ).toBeLessThanOrEqual(6);
    run.note(`run took ${rounds} round(s)`);
    await run.dwell(2400);
    await run.caption("The verb's checked report — verdicts, residue, reframe, and the constitution.", 3600);

    // ── (g) The constitution position: zero untraced figures in [passed]. ──
    const constitution = page.getByTestId("dr-constitution");
    await constitution.waitFor({ state: "visible", timeout: 30_000 });
    const position = (await constitution.textContent())?.trim() ?? "";
    expect(
      position,
      "the run's report must hold the zero-untraced-figures-in-[passed] position " +
        "(either explicit holds or no [passed] claims — never a named violation)",
    ).toMatch(/Position holds|No \[passed\] claims/);
    expect(await page.getByTestId("dr-constitution-violations").count()).toBe(0);
    run.note(`constitution: "${position}"`);
    run.mark("constitution-holds");
    await run.dwell(2000);

    // The checked claims with their corroboration accounting, if any passed.
    const claims = page.locator(".dr-claim");
    const claimCount = await claims.count();
    run.note(`gate returned ${claimCount} claim(s)`);
    if (claimCount > 0) {
      await expect(claims.first()).toContainText(/\S/);
      run.mark("claims-rendered");
    }

    // ── The Library handoff: the completed run's report is findable there. ──
    await demoClick(page, page.getByTestId("dr-open-library"), { settleMs: 800 });
    await expect(page.getByTestId("library-view")).toBeVisible({ timeout: 20_000 });
    run.mark("library-handoff");
    await run.dwell(1800);

    await run.park();
    await run.dwell(2000);
  },
);

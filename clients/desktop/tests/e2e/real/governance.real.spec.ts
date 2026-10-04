// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Real-stack proof of the desktop **governance** surface (FR-9): a house
// steward opens a governed notebook, works its Conflicts tab, and each
// adjudication round-trips through the real Tauri command → the real
// event-sourced oplog → the real `GovernanceView` fold.
//
// The corpus is a REAL ingested folder (charter + minutes) with a
// deterministic, checked-in post-build atlas overlaid onto it by
// global-setup (`plantGovernanceCorpus`) — so the four conflicts are known
// without an LLM enrich, and the assertions are stable. We verify every
// mutation two ways: the UI moves the card, AND `governance_get_view` read
// back over the bridge shows the new disposition in the persisted oplog.
//
// This is the browser-level complement to the deterministic Rust
// command-layer test `full_governance_flow_survives_a_weekly_rebuild`
// (which proves the fold + weekly-rebuild durability). Here the point is
// the seam that test can't reach: the real UI driving the real commands
// against the real running process.
import fs from "node:fs";
import { expect, realBootToChat, test } from "./test-base-real";
import { GOV_FIXTURE_INFO } from "./global-setup";

interface TensionWire {
  why: string | null;
  disposition: { disposition: string };
}
interface ViewWire {
  view: { tensions: TensionWire[] };
}

function dispositionByWhy(v: ViewWire, why: string): string {
  const t = v.view.tensions.find((x) => x.why === why);
  if (!t) throw new Error(`no tension with crux ${JSON.stringify(why)}`);
  return t.disposition.disposition;
}
function openCount(v: ViewWire): number {
  return v.view.tensions.filter((t) => t.disposition.disposition === "open").length;
}

const QUIET = "When do quiet hours begin now?";
const OVERNIGHT = "May a guest stay overnight?";
const KITCHEN = "Who cleans the kitchen?";
const PARKING = "Where do guests park?"; // the lexical decoy — not a real conflict

test("real stack: govern a house notebook — dismiss, resolve, and accept from the Conflicts tab", async ({
  sovereignPage: page,
  bridge,
}) => {
  test.setTimeout(120_000);

  const info = JSON.parse(fs.readFileSync(GOV_FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };
  const corpusId = info.corpus_id;
  const view = () => bridge.invoke<ViewWire>("governance_get_view", { corpusId });

  // ── Baseline: the real command sees four open conflicts. ──
  const v0 = await view();
  expect(openCount(v0)).toBe(4);
  for (const why of [QUIET, OVERNIGHT, KITCHEN, PARKING]) {
    expect(dispositionByWhy(v0, why)).toBe("open");
  }

  // ── Open the governed notebook from the shelf. ──
  await realBootToChat(page);
  await page.getByTestId("nav-library").click();

  const card = page.locator(`[data-notebook-id="${corpusId}"]`);
  await expect(card).toBeVisible();
  await expect(card.getByText(/4 conflicts/)).toBeVisible(); // the shelf chip
  await card.getByTestId("notebook-ask").click();

  // ── Conflicts tab: gated on open_conflicts != null, so it shows here. ──
  await expect(page.getByTestId("notebook-tab-conflicts")).toBeVisible();
  await page.getByTestId("notebook-tab-conflicts").click();
  await expect(page.getByTestId("notebook-tab-conflicts")).toHaveClass(/active/);

  // The four cards render, ranked; each crux is the house question.
  await expect(page.getByTestId("conflict-card")).toHaveCount(4);
  await expect(page.getByText(QUIET)).toBeVisible();

  const cardFor = (crux: string) =>
    page.getByTestId("conflict-card").filter({ hasText: crux });

  // ── 1. Dismiss the decoy — one click, no dialog (steward's call). ──
  await cardFor(PARKING).getByTestId("conflict-dismiss").click();
  await expect(page.getByTestId("conflict-card")).toHaveCount(3);
  await expect
    .poll(async () => dispositionByWhy(await view(), PARKING), { timeout: 8_000 })
    .toBe("dismissed");

  // ── 2. Resolve quiet-hours — keep one rule; the other is superseded. ──
  const quiet = cardFor(QUIET);
  await quiet.getByRole("button", { name: "Keep this rule" }).first().click();
  const rationale = quiet.getByPlaceholder("How was this decided?");
  await expect(rationale).toBeVisible();
  await expect(rationale).toHaveValue(/^Meeting — /); // pre-filled, dated
  await quiet.getByRole("button", { name: "Confirm" }).click();
  await expect(page.getByTestId("conflict-card")).toHaveCount(2);
  await expect
    .poll(async () => dispositionByWhy(await view(), QUIET), { timeout: 8_000 })
    .toBe("resolved");

  // ── 3. Accept the kitchen contradiction — both stand, note required. ──
  const kitchen = cardFor(KITCHEN);
  await kitchen.getByRole("button", { name: "Both can stand" }).click();
  const note = kitchen.getByPlaceholder("Why do both stand? (required)");
  await expect(note).toBeVisible();
  // The note is required: Confirm is disabled until it's filled.
  const kitchenConfirm = kitchen.getByRole("button", { name: "Confirm" });
  await expect(kitchenConfirm).toBeDisabled();
  await note.fill("Both stand — the cook decides by custom.");
  await expect(kitchenConfirm).toBeEnabled();
  await kitchenConfirm.click();
  await expect(page.getByTestId("conflict-card")).toHaveCount(1);
  await expect
    .poll(async () => dispositionByWhy(await view(), KITCHEN), { timeout: 8_000 })
    .toBe("accepted");

  // ── Final ground truth in the persisted oplog + the living history UI. ──
  const vf = await view();
  expect(openCount(vf)).toBe(1);
  expect(dispositionByWhy(vf, OVERNIGHT)).toBe("open"); // never adjudicated
  expect(dispositionByWhy(vf, QUIET)).toBe("resolved");
  expect(dispositionByWhy(vf, KITCHEN)).toBe("accepted");
  expect(dispositionByWhy(vf, PARKING)).toBe("dismissed");

  // Settled (resolved + accepted) and Dismissed collapse-groups reflect it.
  await expect(page.getByText(/Settled \(2\)/)).toBeVisible();
  await expect(page.getByText(/Dismissed \(1\)/)).toBeVisible();

  // The one remaining open conflict is the guest-overnight question.
  await expect(cardFor(OVERNIGHT)).toBeVisible();
});

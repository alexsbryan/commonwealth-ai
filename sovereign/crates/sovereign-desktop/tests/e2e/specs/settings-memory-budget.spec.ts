// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Settings → Models blocks Save when the selected model set would not fit
// in this machine's memory. The consequence of getting it wrong runs in
// both directions, which is why both are asserted here:
//
//   too loose — an over-budget set saves, the daemon OOMs on model load,
//               and the app the user just "configured" no longer answers;
//   too tight — a merely-warned set cannot be saved at all, so a legal
//               configuration is unreachable and the user has no way to
//               tell a warning from a wall.
//
// The band math (`memoryBudget.ts`: warn ≥80%, crit ≥95%) is unit-tested.
// What was NOT tested was the WIRING — that `budgetState` reaches the Save
// button's `disabled`. `sabotage-bank.mjs` moved the block from the crit
// band to the warn band and the whole desktop gate stayed green; it was
// tracked as `hole-memory-budget-guard-band`. Both tests below fail under
// that mutation, one from each side.
//
// Fixtures are chosen so the arithmetic is legible rather than tuned:
//   effective = 16 GiB unified. peak = (fast + embed + max(primary, code))
//               * 1.15 + 2 GiB baseline.
//   ok   → fast 1.0 + embed 0.5, no primary  → 3.7 GiB  = 23%
//   warn → + primary 8.6                     → 13.6 GiB = 85%
//   crit → + primary 12.0                    → 17.5 GiB = 110%

const GIB = 1024 ** 3;

const FAST = "/models/fast-1b.gguf";
const EMBED = "/models/embed-small.gguf";
const PRIMARY_WARN = "/models/mid-13b.gguf";
const PRIMARY_CRIT = "/models/huge-70b.gguf";

const SIZES: Record<string, number> = {
  [FAST]: 1.0 * GIB,
  [EMBED]: 0.5 * GIB,
  [PRIMARY_WARN]: 8.6 * GIB,
  [PRIMARY_CRIT]: 12.0 * GIB,
};

/** Stub the four commands the Models tab needs to compute a real budget.
 *
 *  `detect_hardware` is overridden rather than left to the shim default so
 *  the ratio is fixed by this file: a change to the shared default must not
 *  be able to silently move these cases into a different band. */
async function primeModelsTab(page: Page): Promise<void> {
  await page.addInitScript(
    ({ sizes, fast, embed }) => {
      const w = window as unknown as {
        __sovereign_test__?: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
        __savedSlots__?: unknown;
      };
      const wait = setInterval(() => {
        if (!w.__sovereign_test__) return;
        clearInterval(wait);
        w.__sovereign_test__.setHandler("detect_hardware", () => ({
          system_ram_gb: 16,
          gpu_available: false,
          gpu_name: null,
          gpu_memory_gb: null,
          is_unified_memory: true,
        }));
        w.__sovereign_test__.setHandler("get_setup_model_slots", () => ({
          fast,
          primary: null,
          embed,
          code: null,
          code_family: "Qwen3",
        }));
        w.__sovereign_test__.setHandler("model_file_size", (args) => {
          const { path } = args as { path: string };
          return sizes[path] ?? null;
        });
        // ModelSelector reads `discovered.length` — an unstubbed command
        // resolves undefined and throws the moment a slot is expanded.
        w.__sovereign_test__.setHandler("scan_for_models", () => []);
        w.__sovereign_test__.setHandler("set_setup_model_slots", (args) => {
          w.__savedSlots__ = args;
          return undefined;
        });
      }, 1);
    },
    { sizes: SIZES, fast: FAST, embed: EMBED },
  );
}

async function openModelsTab(page: Page): Promise<void> {
  await page.getByTestId("nav-settings").click();
  await expect(page.locator(".cfg")).toBeVisible();
  await page
    .locator(".cfg-toc .toc-item")
    .filter({ hasText: /^Models$/ })
    .click();
  // Hardware detection is async; the meter only renders once it lands.
  await expect(page.locator(".budget-meter")).toBeVisible();
}

/** Assign `path` to the Main-responder slot through the real picker, so
 *  the whole chain under test runs: select → markSlotsDirty → the size
 *  $effect → peakBytes → budgetState → the Save button's `disabled`. */
async function selectPrimaryModel(page: Page, path: string): Promise<void> {
  await page.locator('.slot-item[data-role="primary"] .slot-item-row').click();
  const body = page.locator('.slot-item[data-role="primary"] .slot-item-body');
  await expect(body).toBeVisible();
  await body.getByRole("button", { name: "Or enter path manually" }).click();
  await body.locator('input[placeholder="/path/to/model.gguf"]').fill(path);
  await body.getByRole("button", { name: "Use", exact: true }).click();
}

const saveButton = (page: Page) =>
  page.getByRole("button", { name: "Save and apply settings" });

test.describe("Settings → Models — memory budget guard band", () => {
  test("an over-budget model set (crit) blocks Save and says why", async ({
    sovereignPage: page,
    chat,
  }) => {
    await primeModelsTab(page);
    await bootToChat(page, chat);
    await openModelsTab(page);

    // Baseline: fast + embed only, comfortably inside the budget.
    await expect(page.locator(".budget-meter")).toHaveClass(/budget-meter--ok/);

    await selectPrimaryModel(page, PRIMARY_CRIT);

    // The meter crosses into crit …
    await expect(page.locator(".budget-meter")).toHaveClass(
      /budget-meter--crit/,
    );
    await expect(page.locator(".budget-meter-pct")).toHaveText("110%");

    // … and the guard fires. The set IS dirty — this is the block, not
    // "nothing to save".
    await expect(saveButton(page)).toBeDisabled();
    await expect(page.locator(".save-msg--error")).toHaveText(
      "Over the memory budget — adjust models above.",
    );
    await expect(saveButton(page)).toHaveAttribute(
      "title",
      "Resolve the memory budget warning above before saving.",
    );

    // Nothing reached the backend.
    expect(
      await page.evaluate(
        () => (window as unknown as { __savedSlots__?: unknown }).__savedSlots__,
      ),
    ).toBeUndefined();
  });

  test("a warned-but-legal set (warn) still saves", async ({
    sovereignPage: page,
    chat,
  }) => {
    await primeModelsTab(page);
    await bootToChat(page, chat);
    await openModelsTab(page);

    await selectPrimaryModel(page, PRIMARY_WARN);

    // Warned, not blocked: the meter counsels, the button still works.
    await expect(page.locator(".budget-meter")).toHaveClass(
      /budget-meter--warn/,
    );
    await expect(page.locator(".budget-meter-pct")).toHaveText("85%");
    await expect(page.locator(".budget-meter-msg")).toContainText(
      "Close to the ceiling",
    );

    await expect(saveButton(page)).toBeEnabled();
    await expect(page.locator(".save-msg--error")).toHaveCount(0);

    await saveButton(page).click();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __savedSlots__?: { slots?: { primary?: string } } })
              .__savedSlots__?.slots?.primary,
        ),
      )
      .toBe(PRIMARY_WARN);
  });

  // A comfortably-fitting set is the case that must keep passing when the
  // guard band is moved — it is what makes the two cases above a SURGICAL
  // kill rather than "the Models tab crashed". It also pins the third arm
  // of the band: no counsel, no block.
  test("a set well inside the budget (ok) neither warns nor blocks", async ({
    sovereignPage: page,
    chat,
  }) => {
    await primeModelsTab(page);
    await bootToChat(page, chat);
    await openModelsTab(page);

    // Assign the SAME small file the fast slot already holds, so the set
    // stays inside the budget while still going dirty.
    await selectPrimaryModel(page, FAST);

    await expect(page.locator(".budget-meter")).toHaveClass(/budget-meter--ok/);
    await expect(page.locator(".budget-meter-msg")).toContainText(
      "Fits comfortably",
    );
    await expect(saveButton(page)).toBeEnabled();
    await expect(page.locator(".save-msg--error")).toHaveCount(0);
  });
});

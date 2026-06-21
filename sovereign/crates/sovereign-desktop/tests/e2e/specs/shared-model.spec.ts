// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// Shared-model settings surface (SharedModelSettings). Pins the frontend
// contract: given the daemon's cluster-health read + this node's config, the
// cluster chip, the degraded banner, the role presets, and the Host consent
// flow render + behave. The Tauri command surface is mocked via the shim —
// a render/behaviour test, not a real-daemon test.

type Role = "consumer" | "anchor" | "host";

interface SmStatus {
  configured: boolean;
  model_id: string | null;
  eligible_anchors: number;
  quorum_anchors: number;
  available: boolean;
  is_host: boolean;
}

async function openSharedModelTab(
  page: Page,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  chat: any,
  status: SmStatus,
  role: Role = "consumer",
): Promise<void> {
  await bootToChat(page, chat);
  // Stateful config: save_config records the latest, get_config returns it, so
  // an applied role survives the component's post-save refresh().
  await page.evaluate(
    ({ status, role }) => {
      const t = window.__sovereign_test__;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const w = window as any;
      w.__cfg = { shared_model_role: role, shared_model_id: status.model_id };
      t.setHandler("get_config", () => w.__cfg);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      t.setHandler("save_config", (args: any) => {
        w.__cfg = args.config;
        return null;
      });
      t.setHandler("get_shared_model_status", () => status);
    },
    { status, role },
  );
  await page.getByTestId("nav-settings").click();
  await page
    .locator(".cfg-toc .toc-item")
    .filter({ hasText: /^Shared model$/ })
    .click();
  await page.locator(".shared-model").waitFor();
}

test.describe("Shared model settings", () => {
  test("renders the cluster chip + role presets, active role highlighted", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSharedModelTab(page, chat, {
      configured: true,
      model_id: "glm-5.2",
      eligible_anchors: 5,
      quorum_anchors: 5,
      available: true,
      is_host: false,
    });
    const sm = page.locator(".shared-model");
    // Cluster chip: model · available · k/N anchors.
    const chip = sm.locator(".state-available");
    await expect(chip).toContainText("glm-5.2");
    await expect(chip).toContainText("available");
    await expect(chip).toContainText("5/5 anchors");
    // Three role presets.
    await expect(sm.locator(".preset", { hasText: "Use it" })).toBeVisible();
    await expect(sm.locator(".preset", { hasText: "Lend my GPU" })).toBeVisible();
    await expect(sm.locator(".preset", { hasText: "Run it here" })).toBeVisible();
    // Consumer role → "Use it" is the active preset.
    await expect(sm.locator(".preset.active")).toHaveText("Use it");
    // No degraded banner while available.
    await expect(sm.locator(".state-degraded")).toHaveCount(0);
  });

  test("forming cluster shows the degraded banner for a consumer", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSharedModelTab(page, chat, {
      configured: true,
      model_id: "glm-5.2",
      eligible_anchors: 3,
      quorum_anchors: 5,
      available: false,
      is_host: false,
    });
    const sm = page.locator(".shared-model");
    await expect(sm.locator(".state-forming")).toContainText("forming 3/5");
    await expect(sm.locator(".state-degraded")).toContainText(
      "answering from your local model",
    );
  });

  test("choosing Host asks for consent, then persists role=host", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSharedModelTab(page, chat, {
      configured: true,
      model_id: "glm-5.2",
      eligible_anchors: 5,
      quorum_anchors: 5,
      available: true,
      is_host: false,
    });
    const sm = page.locator(".shared-model");
    // Clicking "Run it here" does NOT immediately apply — it opens consent.
    await sm.locator(".preset", { hasText: "Run it here" }).click();
    await expect(page.locator(".modal")).toContainText("Run the shared model here?");
    // The role hasn't changed yet (still consumer).
    await expect(sm.locator(".preset.active")).toHaveText("Use it");
    // Confirm → saves role=host; the post-refresh UI shows Host active.
    await page.locator(".modal .action-primary").click();
    await expect(page.locator(".modal")).toHaveCount(0);
    await expect(sm.locator(".preset.active")).toHaveText("Run it here");
    // The persisted config carries the host role.
    const saved = await page.evaluate(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      () => (window as any).__cfg.shared_model_role,
    );
    expect(saved).toBe("host");
  });
});

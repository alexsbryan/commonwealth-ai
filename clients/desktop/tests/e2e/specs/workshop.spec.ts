// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Workshop facets (Phase 3 UX refactor).
//
// The maker tools live under one Workshop roof. Phase 3 re-parents three
// surfaces out of Settings — Connect tools (MCP servers), Open to apps
// (the OpenAI endpoint), and Test (recipe validate/test) — alongside the
// existing Build and Run. This pins that the re-parented facets switch
// and render. (Build/Run are covered by recipe-author-workspace +
// real-workflow-run.)

test.describe("Workshop facets", () => {
  test("the re-parented maker facets switch and render", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-workshop").click();

    await expect(page.getByTestId("workshop-view")).toBeVisible();
    // Build is the default facet.
    await expect(page.getByTestId("workshop-tab-build")).toHaveClass(/active/);

    // Connect tools → the MCP servers section.
    await page.getByTestId("workshop-tab-connect").click();
    await expect(page.getByRole("heading", { name: "Connect tools" })).toBeVisible();

    // Open to apps → the OpenAI-compatible endpoint section (ConnectSection).
    await page.getByTestId("workshop-tab-apps").click();
    await expect(page.getByRole("heading", { name: "Open to apps" })).toBeVisible();
    await expect(page.locator(".connect")).toBeVisible();

    // Test → the recipe tester.
    await page.getByTestId("workshop-tab-test").click();
    await expect(page.getByRole("heading", { name: "Test a recipe" })).toBeVisible();
  });
});

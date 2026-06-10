// SPDX-License-Identifier: AGPL-3.0-or-later
// Settings round-trip against the real config store: the panel renders
// from real state, and a config write through the production command
// persists, reads back, and is restored — all inside the hermetic
// scratch profile, so nothing leaks to the developer's real config.
import { expect, realBootToChat, test } from "./test-base-real";

interface DesktopConfig {
  enable_recipe_authoring: boolean;
  [key: string]: unknown;
}

test("settings panel renders; save_config round-trips and restores", async ({
  sovereignPage: page,
  bridge,
}) => {
  await realBootToChat(page);

  await page.getByTestId("nav-settings").click();
  await expect(page.locator(".cfg")).toBeVisible();

  const original = await bridge.invoke<DesktopConfig>("get_config");
  expect(typeof original.enable_recipe_authoring).toBe("boolean");

  const flipped = { ...original, enable_recipe_authoring: !original.enable_recipe_authoring };
  try {
    await bridge.invoke("save_config", { config: flipped });
    const readBack = await bridge.invoke<DesktopConfig>("get_config");
    expect(readBack.enable_recipe_authoring).toBe(flipped.enable_recipe_authoring);
  } finally {
    await bridge.invoke("save_config", { config: original });
  }
  const restored = await bridge.invoke<DesktopConfig>("get_config");
  expect(restored.enable_recipe_authoring).toBe(original.enable_recipe_authoring);
});

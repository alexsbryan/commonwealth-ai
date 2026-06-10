// SPDX-License-Identifier: AGPL-3.0-or-later
// The glassbox surface itself, against real state: an inner-work
// witness turn captures TurnProvenance (system prompt, memories,
// timings), and the ProvenancePanel (Ctrl+/) renders it. This is the
// only surface where get_last_turn_provenance applies — regular chat
// turns carry their glassbox data on message metadata instead (see
// invariants.ts).
import { expect, realBootToChat, test } from "./test-base-real";

test("inner-work witness turn captures provenance; Ctrl+/ panel renders it", async ({
  sovereignPage: page,
}) => {
  test.setTimeout(240_000);
  await realBootToChat(page);

  await page.getByRole("button", { name: "Inner Work" }).click();
  const entry = page.locator('textarea[aria-label^="Inner work entry"]');
  await entry.waitFor({ state: "visible", timeout: 30_000 });

  await entry.fill(
    "Today I kept circling the same worry about the harvest instead of starting it.",
  );

  // Ctrl+Enter summons the witness — a real streaming turn.
  const before = await page.evaluate(
    () =>
      window.__sovereign_real__.captured.filter((r) => r.event === "message-complete")
        .length,
  );
  await entry.press("Control+Enter");
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter(
              (r) => r.event === "message-complete",
            ).length,
        ),
      { timeout: 180_000, intervals: [1000, 2000] },
    )
    .toBeGreaterThan(before);

  // Ctrl+/ (physical Slash chord) opens the provenance panel, which
  // re-fetches get_last_turn_provenance on every press.
  await page.keyboard.press("Control+Slash");
  const panel = page.locator("aside.provenance");
  await expect(panel).toBeVisible({ timeout: 15_000 });

  // The panel renders a real capture: the "captured …" stamp proves
  // get_last_turn_provenance returned a TurnProvenance, and the
  // "your message" section (expanded by default) carries the actual
  // entry text the witness was sent.
  await expect(panel).toContainText("captured");
  const yourMessage = panel.getByRole("button", { name: /your message/ });
  await expect(yourMessage).toBeVisible();
  if ((await yourMessage.getAttribute("aria-expanded")) === "false") {
    await yourMessage.click();
  }
  await expect(panel).toContainText("circling the same worry");
});

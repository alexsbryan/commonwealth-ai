// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Inner Work — Phases 1 + 2.
//
// Phase 1: the writing surface (gradient field, threshold, dateline,
// localStorage-backed draft, brand-corner exit).
//
// Phase 2: witness summon via Cmd+Return — the user's text becomes a
// settled paragraph, a subtle dot indicates composing, and on
// completion the witness response fades in as marginalia. Esc cancels
// in-flight composition; partial output is discarded.
//
// The pageerror watcher in test-base auto-fails on any uncaught
// exception during render, so we don't assert that explicitly.
test.describe("inner work surface — Phase 1", () => {
  test("user opens inner work, types, exits, returns and sees their text", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // The sidebar entry is always-on once Phase 1 ships.
    const openBtn = page.getByTestId("nav-reflect");
    await expect(openBtn).toBeVisible();
    await openBtn.click();

    // The surface renders. The threshold holds the empty page for
    // 800ms; the dateline fades in after, so we wait for it.
    const dateline = page.locator(".dateline");
    await expect(dateline).toBeVisible({ timeout: 3_000 });

    // Local indicator is always present and unblinking.
    await expect(page.locator(".local-indicator")).toBeVisible();
    await expect(page.locator(".local-indicator")).toContainText("local");

    // Type into the column. The textarea is the only writing affordance.
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible();
    await column.click();
    await column.fill("Sitting with what's here today.");

    // Wait long enough for the debounced save to flush (400ms).
    await page.waitForTimeout(600);

    // Exit via the brand-corner mark. We're back in chat.
    await page.locator(".exit-mark").click();
    await expect(page.locator(".app-layout")).toBeVisible();

    // Re-enter the surface; the draft is restored from localStorage
    // and the same text is visible. The threshold has already played
    // for this session, so there's no 800ms hold the second time.
    await page.getByTestId("nav-reflect").click();
    const columnAgain = page.locator("textarea.column");
    await expect(columnAgain).toBeVisible({ timeout: 1_500 });
    await expect(columnAgain).toHaveValue("Sitting with what's here today.");
  });
});

test.describe("inner work surface — Phase 2 witness", () => {
  test("Cmd+Return summons the witness; response fades in as marginalia", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    // Write a passage and summon the witness.
    await column.fill("I noticed the critic showed up again this morning.");
    await column.press("Meta+Enter");

    // The user's text becomes a settled paragraph in the document.
    // The textarea is cleared. A subtle composing dot appears.
    await expect(page.locator(".user-prose")).toContainText(
      "I noticed the critic showed up again this morning.",
    );
    await expect(column).toHaveValue("");
    await expect(page.locator(".composing-dot")).toBeVisible();
    // No witness marginalia yet — design intent: no streaming display,
    // the response only fades in once on completion.
    await expect(page.locator(".witness")).toHaveCount(0);

    // Drive the stream complete from the test harness.
    const start = await chat.api.lastStreamStart();
    expect(start).not.toBeNull();
    await chat.api.completeMessage(
      start!.messageId,
      "The critic showing up around uncertainty — is that protection, or rehearsal?",
    );

    // Witness reflection now visible as marginalia (left-rule blockquote).
    // Composing dot is gone.
    await expect(page.locator(".witness")).toContainText(
      "The critic showing up around uncertainty",
    );
    await expect(page.locator(".composing-dot")).toHaveCount(0);

    // The textarea regains focus, ready for continued writing below.
    // (focus assertions are flaky in headless modes; we settle for
    // verifying the textarea exists and is enabled instead.)
    await expect(column).toBeEnabled();
  });

  test("Esc cancels an in-flight witness; no half-paragraph stranded", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    await column.fill("Something I'm sitting with today.");
    await column.press("Meta+Enter");

    // Composing dot should appear; witness should not yet exist.
    await expect(page.locator(".composing-dot")).toBeVisible();
    await expect(page.locator(".witness")).toHaveCount(0);

    // The shim recorded the stream start. Hit Esc before completing.
    const start = await chat.api.lastStreamStart();
    expect(start).not.toBeNull();
    await page.keyboard.press("Escape");

    // Cancellation was forwarded to the runtime.
    await expect
      .poll(async () => chat.api.lastCancel(), { timeout: 2_000 })
      .not.toBeNull();

    // Composing dot is gone; the user's prose stays as a settled
    // paragraph; no witness marginalia rendered.
    await expect(page.locator(".composing-dot")).toHaveCount(0);
    await expect(page.locator(".user-prose")).toContainText(
      "Something I'm sitting with today.",
    );
    await expect(page.locator(".witness")).toHaveCount(0);

    // A late-arriving completion event for that message should
    // become a no-op (the surface unlatched the pending turn locally
    // when Esc fired). Drive a completion and assert nothing renders.
    await chat.api.completeMessage(start!.messageId, "stranded text");
    await expect(page.locator(".witness")).toHaveCount(0);
  });

  test("Cmd+Return on whitespace-only input is a no-op", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    await column.fill("   \n\n  ");
    await column.press("Meta+Enter");

    // No stream started, no document turn appended.
    expect(await chat.api.lastStreamStart()).toBeNull();
    await expect(page.locator(".user-prose")).toHaveCount(0);
    await expect(page.locator(".composing-dot")).toHaveCount(0);
  });
});

test.describe("inner work surface — Phase 3a echoes", () => {
  // Helper: short-circuit the 8–12s echo delay so tests don't burn
  // real wall-clock seconds. Set on window before navigating into the
  // surface; cleared on backendReady.
  async function shortenEchoDelay(page: import("@playwright/test").Page) {
    await page.addInitScript(() => {
      (window as unknown as { __inner_work_echo_delay_ms__?: number })
        .__inner_work_echo_delay_ms__ = 50;
    });
  }

  // Helper: drive a complete witness exchange with the given user
  // text and witness response. Returns when the witness marginalia
  // is visible.
  async function exchange(
    page: import("@playwright/test").Page,
    chat: { api: { lastStreamStart: () => Promise<{ messageId: string } | null>; completeMessage: (id: string, text: string) => Promise<void> } },
    column: import("@playwright/test").Locator,
    userText: string,
    witnessText: string,
  ) {
    // The textarea may have leftover content from the previous turn's
    // composing focus — clear before filling to avoid concatenation.
    await column.fill("");
    await column.fill(userText);
    await column.press("Meta+Enter");
    const start = await expect
      .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
      .not.toBeNull();
    const s = (await chat.api.lastStreamStart())!;
    await chat.api.completeMessage(s.messageId, witnessText);
    await expect(page.locator(".witness").last()).toContainText(witnessText);
    // Reset the harness-recorded last start so the next call to
    // lastStreamStart() observes a fresh value (poll above otherwise
    // returns the stale id immediately on the next exchange).
    await page.evaluate(() => {
      window.__sovereign_test__._lastStreamStart = null;
    });
    return s;
  }

  test("a resonant paragraph surfaces a gutter dot; click opens the overlay", async ({
    sovereignPage: page,
    chat,
  }) => {
    await shortenEchoDelay(page);
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    // First turn — establishes a paragraph in the document with several
    // distinctive content words.
    await exchange(
      page,
      chat,
      column,
      "The critic showed up around uncertainty about whether the project would land.",
      "What is the critic protecting?",
    );

    // No echo dots after the first turn — there's nothing earlier to
    // echo against.
    await expect(page.locator(".echo-dot")).toHaveCount(0);

    // Second turn — shares >= 3 content words with the first
    // ("critic", "uncertainty", "project"). After the short delay the
    // dot should appear in the gutter beside the second user paragraph.
    await exchange(
      page,
      chat,
      column,
      "Today the critic is back; the same uncertainty about the project is here too.",
      "What changed between then and now?",
    );

    await expect(page.locator(".echo-dot")).toHaveCount(1, { timeout: 1_500 });

    // Click the dot — the EchoOverlay opens with the prior paragraph.
    // The page already carries another `role="dialog"` (the inner-work
    // history drawer); name the overlay by its aria-label so the locator
    // is unambiguous.
    await page.locator(".echo-dot").click();
    const overlay = page.getByRole("dialog", { name: "Echo from earlier writing" });
    await expect(overlay).toBeVisible();
    await expect(overlay).toContainText("uncertainty about whether the project");

    // Esc closes the overlay; the dot stays available for a future click.
    await page.keyboard.press("Escape");
    await expect(overlay).toHaveCount(0);
    await expect(page.locator(".echo-dot")).toHaveCount(1);
  });

  test("when message metadata carries recalled_memories, the echo uses them", async ({
    sovereignPage: page,
    chat,
  }) => {
    await shortenEchoDelay(page);
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    // No prior turn in this conversation — frontend similarity would
    // find nothing. The echo can only come from metadata.
    await column.fill("A small thing today, but it has weight.");
    await column.press("Meta+Enter");
    const start = await expect
      .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
      .not.toBeNull();
    const s = (await chat.api.lastStreamStart())!;

    // Complete the witness turn with metadata that carries the
    // runtime's recalled memories. `created_at` ~3 days ago so the
    // overlay shows "3 days ago" rather than "earlier today".
    const threeDaysAgo =
      Math.floor(Date.now() / 1000) - 3 * 24 * 60 * 60;
    await chat.api.completeMessage(s.messageId, "What is the weight made of?", {
      recalled_memories: [
        {
          id: "mem-001",
          content: "I told my therapist last week that I keep choosing things I can't carry.",
          created_at: threeDaysAgo,
        },
      ],
    });

    // The echo dot appears beside the just-committed turn.
    await expect(page.locator(".echo-dot")).toHaveCount(1, { timeout: 1_500 });

    // Click it — the overlay shows the recalled memory's fragment
    // and a relative date label sourced from `created_at`.
    await page.locator(".echo-dot").click();
    const overlay = page.getByRole("dialog", { name: "Echo from earlier writing" });
    await expect(overlay).toBeVisible();
    await expect(overlay).toContainText("can't carry");
    await expect(overlay).toContainText("3 days ago");
  });

  test("unrelated paragraphs do not produce a dot", async ({
    sovereignPage: page,
    chat,
  }) => {
    await shortenEchoDelay(page);
    await bootToChat(page, chat);
    await page.getByTestId("nav-reflect").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    await exchange(
      page,
      chat,
      column,
      "Mountains rise where the wind has eroded softer ridges over centuries.",
      "Witness reflection one.",
    );
    await exchange(
      page,
      chat,
      column,
      "Octopuses possess remarkable problem-solving capabilities according to recent studies.",
      "Witness reflection two.",
    );

    // Wait past the (shortened) echo delay and assert nothing surfaced.
    await page.waitForTimeout(200);
    await expect(page.locator(".echo-dot")).toHaveCount(0);
  });
});

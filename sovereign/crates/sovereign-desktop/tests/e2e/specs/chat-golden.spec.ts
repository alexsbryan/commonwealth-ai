import { test, expect, bootToChat } from "../fixtures/test-base";

// Golden path. If this test ever fails, every other chat test is moot —
// the harness itself is broken. Keep it minimal and stable: one user
// message, a few streamed tokens, completion. No edge cases here.
test.describe("chat golden path", () => {
  test("user sends a message, sees streaming tokens, then completion", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // The empty state shows a SOVEREIGN headline before any messages exist.
    await expect(page.locator(".empty-state")).toBeVisible();

    // Type into the chat input and send.
    const input = page.locator(".input-area textarea");
    await input.fill("hello sovereign");
    await page.locator(".send-btn").click();

    // The shim records the message_id assigned to the assistant
    // placeholder. Pick it up after send_message_stream has resolved.
    const started = await expect
      .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
      .not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // User bubble renders verbatim. Assistant bubble exists as an
    // empty placeholder (.sv-ai-msg) waiting for chunks.
    await expect(page.locator(".bubble.user .content")).toHaveText(
      "hello sovereign",
    );
    await expect(page.locator(".sv-ai-msg")).toBeVisible();

    // Stream a small set of tokens. Word-buffered, so trailing space
    // is required for them to flush.
    await chat.api.streamTokens(
      start.messageId,
      ["hi ", "there ", "human "],
      0,
    );
    await chat.api.completeMessage(start.messageId, "hi there human");

    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "hi there human",
    );

    // Input is re-enabled, Send button is back (the Stop button is
    // only present mid-stream).
    await expect(input).toBeEnabled();
    await expect(page.locator(".send-btn")).toBeVisible();
    await expect(page.locator(".stop-btn")).toHaveCount(0);
  });

  test("Enter submits without Shift; Shift+Enter inserts a newline", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    const input = page.locator(".input-area textarea");

    // Shift+Enter should NOT submit. We assert by checking that the
    // textarea contains a newline and no stream was started.
    await input.fill("line one");
    await input.press("Shift+Enter");
    await input.type("line two");
    expect(await input.inputValue()).toContain("\n");
    expect(await chat.api.lastStreamStart()).toBeNull();

    // Plain Enter submits. Clear and try.
    await input.fill("plain submit");
    await input.press("Enter");
    await expect
      .poll(async () => chat.api.lastStreamStart())
      .not.toBeNull();
  });

  test("Send is disabled until input has non-whitespace content", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    const input = page.locator(".input-area textarea");
    const send = page.locator(".send-btn");

    await expect(send).toBeDisabled();
    await input.fill("   ");
    await expect(send).toBeDisabled();
    await input.fill("a real message");
    await expect(send).toBeEnabled();
  });
});

// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import { MAX_TURN_MESSAGE_CHARS } from "../../../src/lib/types";

// Edge cases — error paths, mid-stream lifecycle, recovery. The
// chat surface lives or dies on whether these recover gracefully;
// a turn that gets stuck is the worst possible UX, far worse than
// a turn that errors out cleanly.

test.describe("chat error and recovery", () => {
  // message-error during a stream should: (1) end the loading state,
  // (2) put Send back, (3) leave the user message + the partial
  // assistant content with an "Error: ..." tail visible.
  test("message-error mid-stream recovers to idle with error appended", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("trigger");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // Stream a couple of words then break.
    await chat.api.streamTokens(start.messageId, ["partial ", "answer "], 0);
    await chat.api.errorMessage("backend exploded");

    await expect(page.locator(".send-btn")).toBeVisible();
    await expect(page.locator(".stop-btn")).toHaveCount(0);
    await expect(page.locator(".input-area textarea")).toBeEnabled();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Error: backend exploded",
    );
  });

  // send_message_stream itself can throw (e.g., daemon 503). The
  // ChatView catch-block synthesises a single "Error: ..." assistant
  // message rather than leaving the user staring at a stuck spinner.
  test("send_message_stream rejection appears as an error bubble", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Override send_message_stream to reject.
    await page.evaluate(() => {
      window.__sovereign_test__.setHandler("send_message_stream", () => {
        throw new Error("daemon unavailable");
      });
    });

    await page.locator(".input-area textarea").fill("knock knock");
    await page.locator(".send-btn").click();

    await expect(page.locator(".bubble.user .content")).toHaveText("knock knock");
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "daemon unavailable",
    );
    // Loading state must clear: Stop is gone, Send is back. Input is
    // empty so Send is correctly disabled until the user types again.
    await expect(page.locator(".stop-btn")).toHaveCount(0);
    await expect(page.locator(".send-btn")).toBeVisible();
    await page.locator(".input-area textarea").fill("retry");
    await expect(page.locator(".send-btn")).toBeEnabled();
  });

  // Conversation switch mid-stream: clicking a different conversation in
  // the sidebar must clear the in-flight stream's spinner. Without the
  // per-substate HYDRATE override the streaming spinner leaks across
  // conversations. We seed two conversations into list_conversations so
  // the sidebar renders something clickable.
  test("conversation switch mid-stream clears spinner cleanly", async ({
    sovereignPage: page,
    chat,
  }) => {
    // Seed the sidebar with two conversations BEFORE boot so the list
    // renders them on first mount. ConversationList calls list_conversations
    // on mount and on `loadConversations()` invocations.
    await page.addInitScript(() => {
      // The shim is already injected by the fixture; override one default.
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("list_conversations", () => [
          {
            id: "conv-a",
            title: "Conversation A",
            created_at: 1,
            updated_at: 1,
            message_count: 0,
          },
          {
            id: "conv-b",
            title: "Conversation B",
            created_at: 2,
            updated_at: 2,
            message_count: 0,
          },
        ]);
      }, 1);
    });

    await bootToChat(page, chat);
    // Both conversations should appear in the sidebar.
    await expect(page.getByText("Conversation A")).toBeVisible();
    await expect(page.getByText("Conversation B")).toBeVisible();

    // Open A, send a message, leave it streaming (no completion).
    await page.getByText("Conversation A").click();
    await page.locator(".input-area textarea").fill("hang");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    await expect(page.locator(".stop-btn")).toBeVisible();

    // Switch to B mid-stream.
    await page.getByText("Conversation B").click();

    // No spinner leak. No stop button. Empty messages region.
    await expect(page.locator(".stop-btn")).toHaveCount(0);
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
    await expect(page.locator(".bubble.user")).toHaveCount(0);
  });
});

test.describe("input area constraints", () => {
  // Oversize paste: above MAX_TURN_MESSAGE_CHARS the Send button is
  // disabled and the oversize hint appears with an "Attach file
  // instead" affordance. This is the chat-doesn't-fire-a-doomed-request
  // guarantee.
  test("oversize input disables Send and shows the attach-instead hint", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    const textarea = page.locator(".input-area textarea");
    const send = page.locator(".send-btn");

    const safe = "x".repeat(MAX_TURN_MESSAGE_CHARS - 10);
    await textarea.fill(safe);
    await expect(send).toBeEnabled();
    await expect(page.locator(".oversize-hint")).toHaveCount(0);

    const oversized = "x".repeat(MAX_TURN_MESSAGE_CHARS + 100);
    await textarea.fill(oversized);
    await expect(send).toBeDisabled();
    await expect(page.locator(".oversize-hint")).toBeVisible();
    await expect(page.locator(".oversize-attach-btn")).toBeVisible();
  });
});

test.describe("streaming lifecycle", () => {
  // Word-buffer residue must flush on completion, even if it doesn't
  // end on a word boundary. Otherwise the last partial word is lost.
  test("trailing partial word flushes on message-complete", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("flush");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    // Final chunk has no trailing space, so the word-buffer is
    // holding "tail" when complete fires.
    await chat.api.streamTokens(
      start.messageId,
      ["leading ", "middle ", "tail"],
      0,
    );
    // Don't pass fullText — verifies the pendingText path (residue
    // appended) rather than the fallback fullText path.
    await chat.api.completeMessage(start.messageId, "");
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText("tail");
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "leading middle tail",
    );
  });

  // Late chunks (arriving after MESSAGE_COMPLETE) must be ignored —
  // the FSM guards on streamingMessageId, which is null after complete.
  // If they leaked through they'd corrupt content of the next assistant
  // message in the conversation.
  test("late chunks after completion are ignored", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.locator(".input-area textarea").fill("once");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;

    await chat.api.streamTokens(start.messageId, ["done "], 0);
    await chat.api.completeMessage(start.messageId, "done");
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText("done");

    // Inject a late chunk for the same message id. It must NOT change
    // the rendered content (FSM is in idle and the guard fails).
    await chat.api.streamTokens(start.messageId, ["LATE_LEAK "], 0);
    await page.waitForTimeout(50);
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "LATE_LEAK",
    );
  });

  // After completion we should be able to fire a second turn that
  // produces its own placeholder + stream. Two assistant bubbles end
  // up on screen.
  test("can send a follow-up turn after completion", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    const input = page.locator(".input-area textarea");

    await input.fill("first");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const first = (await chat.api.lastStreamStart())!;
    await chat.api.streamTokens(first.messageId, ["one "], 0);
    await chat.api.completeMessage(first.messageId, "one");

    await input.fill("second");
    await page.locator(".send-btn").click();
    await expect
      .poll(async () => chat.api.lastStreamStart())
      .toMatchObject({ messageId: expect.not.stringMatching(first.messageId) });
    const second = (await chat.api.lastStreamStart())!;
    await chat.api.streamTokens(second.messageId, ["two "], 0);
    await chat.api.completeMessage(second.messageId, "two");

    // Two user bubbles, two assistant bubbles.
    await expect(page.locator(".bubble.user")).toHaveCount(2);
    await expect(page.locator(".sv-ai-msg")).toHaveCount(2);
  });
});

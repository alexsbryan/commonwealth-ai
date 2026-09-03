// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Regression: a streaming turn must survive the user navigating to
// another conversation and back.
//
// Reported failure (slow mesh peer): ask a question, wait, switch
// conversations, switch back — the question is on screen but there is
// no answer and NO loading affordance, and the answer never lands even
// after the backend finishes.
//
// Root cause (two cooperating defects):
//   1. chat.machine's `streamingMessageId` is wiped to null on HYDRATE
//      (every conversation switch), and the global message-chunk /
//      message-complete events are dropped by the `messageId ===
//      streamingMessageId` guard — even though those events carry a
//      `conversation_id` that says exactly which conversation they
//      belong to.
//   2. The backend persists the assistant row only AFTER the stream is
//      exhausted (StreamHandle contract), so on return `get_conversation`
//      has no assistant row yet — nothing to show, no spinner.
//
// The fix re-attaches an in-flight turn per conversation from the
// live-turns registry (fed by the conversation_id-tagged global
// events), restoring the affordance + partial text on return and
// finalizing the answer even when it lands while the user is away.
//
// These specs emit production-shaped events (WITH conversation_id — the
// shim's streamTokens/completeMessage convenience helpers omit it, but
// commands/chat.rs always sends it) so they exercise the real routing.
// Sidebar titles ("Philosophy chat" / "Bravo topic") are chosen NOT to
// appear in any message bubble, so a title selector stays unambiguous.

test.describe("orphaned streaming turn (navigate away & back)", () => {
  const A = "conv-A";
  const A_TITLE = "Philosophy chat";
  const B_TITLE = "Bravo topic";
  const QUESTION = "Is free will compatible with determinism";

  // Seed two selectable conversations. `get_conversation(A)` returns
  // ONLY the user turn (via the __convAMessages flag) — faithful to the
  // backend persisting the assistant row only after the stream ends. So
  // a correct fix must recover the turn from the live-turns registry,
  // not from the store.
  async function seed(page: import("@playwright/test").Page) {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        const t = window.__sovereign_test__;
        t.setHandler("list_conversations", () => [
          {
            id: "conv-A",
            title: "Philosophy chat",
            created_at: 2,
            updated_at: 2,
            message_count: 1,
          },
          {
            id: "conv-B",
            title: "Bravo topic",
            created_at: 1,
            updated_at: 1,
            message_count: 1,
          },
        ]);
        t.setHandler(
          "get_conversation",
          ({ conversationId }: { conversationId: string }) => {
            if (conversationId === "conv-B") {
              return {
                id: "conv-B",
                title: "Bravo topic",
                created_at: 0,
                updated_at: 0,
                messages: [
                  {
                    id: "b-msg",
                    role: "assistant",
                    content: "BRAVO-CONTENT",
                    created_at: 0,
                  },
                ],
              };
            }
            return {
              id: "conv-A",
              title: "Philosophy chat",
              created_at: 0,
              updated_at: 0,
              messages:
                (window as unknown as { __convAMessages?: unknown[] })
                  .__convAMessages ?? [],
            };
          },
        );
      }, 1);
    });
  }

  // Flip get_conversation(A) to return the persisted USER turn (the
  // assistant row is intentionally absent — persist-at-end).
  async function persistUserTurn(page: import("@playwright/test").Page) {
    await page.evaluate((q) => {
      (window as unknown as { __convAMessages?: unknown[] }).__convAMessages = [
        { id: "a-user", role: "user", content: q, created_at: 0 },
      ];
    }, QUESTION);
  }

  function emitChunk(
    page: import("@playwright/test").Page,
    messageId: string,
    chunk: string,
  ) {
    return page.evaluate(
      ({ cid, mid, c }) => {
        window.__sovereign_test__.emit("message-chunk", {
          conversation_id: cid,
          message_id: mid,
          chunk: c,
        });
      },
      { cid: A, mid: messageId, c: chunk },
    );
  }

  function emitComplete(
    page: import("@playwright/test").Page,
    messageId: string,
    fullText: string,
  ) {
    return page.evaluate(
      ({ cid, mid, f }) => {
        window.__sovereign_test__.emit("message-complete", {
          conversation_id: cid,
          message_id: mid,
          full_text: f,
          metadata: null,
        });
      },
      { cid: A, mid: messageId, f: fullText },
    );
  }

  function emitError(
    page: import("@playwright/test").Page,
    messageId: string,
    message: string,
  ) {
    return page.evaluate(
      ({ cid, mid, m }) => {
        window.__sovereign_test__.emit("message-error", {
          conversation_id: cid,
          message_id: mid,
          message: m,
        });
      },
      { cid: A, mid: messageId, m: message },
    );
  }

  // Click a sidebar conversation by title (scoped to .convo-item so it
  // never collides with message-bubble text).
  function openConversation(
    page: import("@playwright/test").Page,
    title: string,
  ) {
    return page.locator(".convo-item", { hasText: title }).click();
  }

  async function startTurnInA(
    page: import("@playwright/test").Page,
    chat: import("../fixtures/test-base").ChatHarness,
  ): Promise<string> {
    // Select A (empty), then send the question so the turn binds to A.
    await openConversation(page, A_TITLE);
    await page.locator(".input-area textarea").fill(QUESTION);
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;
    // The backend has now persisted the user turn.
    await persistUserTurn(page);
    return start.messageId;
  }

  test("returning mid-stream restores the loading affordance and partial text", { tag: ["@UI-8"] }, async ({
    sovereignPage: page,
    chat,
  }) => {
    await seed(page);
    await bootToChat(page, chat);
    const mid = await startTurnInA(page, chat);

    // Partial tokens arrive for A's turn.
    await emitChunk(page, mid, "Compatibilism ");
    await emitChunk(page, mid, "says yes. ");
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Compatibilism says yes.",
    );

    // Detour to B.
    await openConversation(page, B_TITLE);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "BRAVO-CONTENT",
    );

    // The backend keeps streaming A's turn while we're away.
    await emitChunk(page, mid, "Hard determinists disagree.");

    // Return to A. The turn is still in flight → a loading affordance
    // must be shown, and the partial answer accumulated so far restored.
    await openConversation(page, A_TITLE);
    await expect(
      page.locator(".typing-indicator, .doc-progress-indicator").first(),
    ).toBeVisible();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Compatibilism says yes.",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Hard determinists disagree.",
    );

    // Completion lands while A is active → answer finalized, spinner gone.
    await emitComplete(
      page,
      mid,
      "Compatibilism says yes. Hard determinists disagree.",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Compatibilism says yes. Hard determinists disagree.",
    );
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
  });

  test("an answer that completes while away is shown on return", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seed(page);
    await bootToChat(page, chat);
    const mid = await startTurnInA(page, chat);

    await emitChunk(page, mid, "Compatibilism reconciles them. ");

    // Detour to B, and the whole turn finishes while we're away.
    await openConversation(page, B_TITLE);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "BRAVO-CONTENT",
    );
    await emitChunk(page, mid, "That is the mainstream view.");
    await emitComplete(
      page,
      mid,
      "Compatibilism reconciles them. That is the mainstream view.",
    );

    // Return to A — the completed answer must be visible, no spinner,
    // even though the store (get_conversation) has no assistant row.
    await openConversation(page, A_TITLE);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "Compatibilism reconciles them. That is the mainstream view.",
    );
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
  });

  test("a turn that fails while away surfaces the error on return", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seed(page);
    await bootToChat(page, chat);
    const mid = await startTurnInA(page, chat);

    await emitChunk(page, mid, "Let me think about ");

    // Detour to B; the mesh peer then dies mid-stream.
    await openConversation(page, B_TITLE);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "BRAVO-CONTENT",
    );
    await emitError(page, mid, "peer connection reset");

    // Return to A — the failure must be visible (not a silently blank
    // turn), and no spinner should be stuck on.
    await openConversation(page, A_TITLE);
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "peer connection reset",
    );
    await expect(page.locator(".typing-indicator")).toHaveCount(0);
    await expect(
      page.locator('.doc-progress-indicator[data-source="placeholder"]'),
    ).toHaveCount(0);
  });
});

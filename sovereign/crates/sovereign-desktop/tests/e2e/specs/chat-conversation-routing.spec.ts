// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Conversation routing — sidebar selection and "New conversation"
// button must drive the chat surface. Two regressions guarded here:
//
//   1) Switching conversations: clicking a different sidebar item
//      must replace the visible message list with that conversation's
//      history. Without re-rendering on prop change the user is
//      stuck on whichever conversation loaded first.
//
//   2) New conversation: clicking "New conversation" while in a
//      populated chat must clear the chat surface and bind to the
//      freshly-created id, even though `create_conversation` doesn't
//      persist the row until the first message is sent (so
//      `get_conversation` will throw).

test.describe("conversation routing", () => {
  // Helper: seed two pre-existing conversations with distinct content
  // so we can assert which one the chat is currently rendering.
  async function seedTwoConversations(page: import("@playwright/test").Page) {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("list_conversations", () => [
          {
            id: "conv-alpha",
            title: "Conversation Alpha",
            created_at: 1,
            updated_at: 1,
            message_count: 1,
          },
          {
            id: "conv-bravo",
            title: "Conversation Bravo",
            created_at: 2,
            updated_at: 2,
            message_count: 1,
          },
        ]);
        window.__sovereign_test__.setHandler(
          "get_conversation",
          ({ conversationId }: { conversationId: string }) => ({
            id: conversationId,
            title:
              conversationId === "conv-alpha"
                ? "Conversation Alpha"
                : "Conversation Bravo",
            messages: [
              {
                id: `${conversationId}-msg`,
                role: "assistant",
                content:
                  conversationId === "conv-alpha"
                    ? "ALPHA-CONTENT"
                    : "BRAVO-CONTENT",
                created_at: 0,
              },
            ],
            created_at: 0,
            updated_at: 0,
          }),
        );
      }, 1);
    });
  }

  test("clicking a sidebar item replaces the visible chat content", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedTwoConversations(page);
    await bootToChat(page, chat);

    // Click Alpha first — chat should render Alpha's seeded message.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "BRAVO-CONTENT",
    );

    // Switch to Bravo. The chat must replace Alpha's content with
    // Bravo's, not stay stuck on Alpha.
    await page.getByText("Conversation Bravo").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "BRAVO-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "ALPHA-CONTENT",
    );

    // And back to Alpha — same invariant the other direction.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "BRAVO-CONTENT",
    );
  });

  // The "New conversation" button case: a freshly-created conversation
  // doesn't yet exist in the SQLite store (create_conversation just
  // mints a UUID), so get_conversation throws. ChatView's catch arm
  // hydrates with an empty messages array — the chat surface MUST
  // visibly clear, not stay stuck on whichever convo was open.
  test("clicking 'New conversation' clears the chat from a populated one", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedTwoConversations(page);
    await bootToChat(page, chat);

    // Open Alpha so the chat is populated.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );

    // After clicking the New-conversation button the freshly-minted
    // id won't have a row yet — get_conversation will throw the shim's
    // default "conversation X not found". Override get_conversation to
    // throw for any unknown id (ours is a fresh UUID).
    await page.evaluate(() => {
      const known = new Set(["conv-alpha", "conv-bravo"]);
      window.__sovereign_test__.setHandler(
        "get_conversation",
        ({ conversationId }: { conversationId: string }) => {
          if (known.has(conversationId)) {
            return {
              id: conversationId,
              title:
                conversationId === "conv-alpha"
                  ? "Conversation Alpha"
                  : "Conversation Bravo",
              messages: [
                {
                  id: `${conversationId}-msg`,
                  role: "assistant",
                  content:
                    conversationId === "conv-alpha"
                      ? "ALPHA-CONTENT"
                      : "BRAVO-CONTENT",
                  created_at: 0,
                },
              ],
              created_at: 0,
              updated_at: 0,
            };
          }
          throw new Error(`conversation ${conversationId} not found`);
        },
      );
    });

    await page.locator(".new-btn").click();

    // Empty state should be back; the previous conversation's content
    // must be gone from the messages region.
    await expect(page.locator(".sv-ai-msg .sv-prose")).toHaveCount(0);
    await expect(page.locator(".bubble")).toHaveCount(0);
  });

  // Eager clear on switch: when the user clicks a different
  // conversation, the previous conversation's bubbles MUST disappear
  // immediately, even before `get_conversation` resolves. Without
  // this guarantee the chat appears "stuck on the first conversation
  // to load" while a slow backend round-trip is in flight — that
  // perceived stickiness is the most-reported flavor of this bug.
  test("switching conversations clears the prior content before the fetch resolves", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("list_conversations", () => [
          {
            id: "conv-x",
            title: "Conv X",
            created_at: 1,
            updated_at: 1,
            message_count: 1,
          },
          {
            id: "conv-y",
            title: "Conv Y",
            created_at: 2,
            updated_at: 2,
            message_count: 1,
          },
        ]);
        // Y intentionally hangs forever — the eager clear must
        // still kick in based on the click, not the fetch.
        const xPayload = {
          id: "conv-x",
          title: "Conv X",
          messages: [
            {
              id: "x-msg",
              role: "assistant",
              content: "X-VISIBLE-CONTENT",
              created_at: 0,
            },
          ],
          created_at: 0,
          updated_at: 0,
        };
        window.__sovereign_test__.setHandler(
          "get_conversation",
          ({ conversationId }: { conversationId: string }) => {
            if (conversationId === "conv-x") return xPayload;
            // Conv Y: never resolves — represents a slow / hung backend.
            return new Promise(() => {});
          },
        );
      }, 1);
    });

    await bootToChat(page, chat);

    await page.getByText("Conv X").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "X-VISIBLE-CONTENT",
    );

    // Click Y — its fetch never resolves. The eager-clear behavior
    // must drop X's content within ~100ms anyway.
    await page.getByText("Conv Y").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toHaveCount(0, {
      timeout: 500,
    });
    await expect(page.locator(".bubble")).toHaveCount(0);
  });

  // Race: get_conversation responses arriving out of order. If A's
  // fetch resolves LATER than B's, naively-coded HYDRATE plumbing
  // ends up displaying A even though B was the user's final choice.
  // The fix is to drop responses whose target no longer matches the
  // currently-selected conversationId. Without that fix this test
  // pins the bug.
  test("out-of-order get_conversation responses don't override the latest selection", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("list_conversations", () => [
          {
            id: "conv-slow",
            title: "Conv Slow",
            created_at: 1,
            updated_at: 1,
            message_count: 1,
          },
          {
            id: "conv-fast",
            title: "Conv Fast",
            created_at: 2,
            updated_at: 2,
            message_count: 1,
          },
        ]);
        // SLOW path always lags FAST. Clicking SLOW first then FAST
        // means we want FAST's content to win, even though SLOW's
        // response arrives later.
        window.__sovereign_test__.setHandler(
          "get_conversation",
          ({ conversationId }: { conversationId: string }) =>
            new Promise((resolve) => {
              const delayMs = conversationId === "conv-slow" ? 250 : 30;
              setTimeout(() => {
                resolve({
                  id: conversationId,
                  title:
                    conversationId === "conv-slow" ? "Conv Slow" : "Conv Fast",
                  messages: [
                    {
                      id: `${conversationId}-msg`,
                      role: "assistant",
                      content:
                        conversationId === "conv-slow"
                          ? "SLOW-CONTENT"
                          : "FAST-CONTENT",
                      created_at: 0,
                    },
                  ],
                  created_at: 0,
                  updated_at: 0,
                });
              }, delayMs);
            }),
        );
      }, 1);
    });

    await bootToChat(page, chat);

    // Click slow first, then fast — fast should be the final winner.
    await page.getByText("Conv Slow").click();
    await page.getByText("Conv Fast").click();

    // Wait long enough for the slow response to land. If the racing
    // bug isn't fixed, slow's HYDRATE will overwrite fast's content.
    await page.waitForTimeout(400);

    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "FAST-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "SLOW-CONTENT",
    );
  });

  // Regression: starter-question round-robin pulls atom_ids that
  // collide across corpora (every atlas restarts numbering at
  // `question-0001`). Before the StarterChips key fix, those
  // duplicates crashed Svelte's keyed-each with `each_key_duplicate`
  // — which froze ChatView's reactive subtree and made conversation
  // switching feel "stuck". The console-error gate in test-base.ts
  // catches the runtime diagnostic; this test reproduces the exact
  // scenario that triggered it.
  test("colliding atom_ids across corpora don't crash the starter chips", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("enrich_list_corpora", () => [
          { corpus_id: "corpus-a", display_name: "Alpha" },
          { corpus_id: "corpus-b", display_name: "Bravo" },
        ]);
        // Both corpora return atom_id "question-0001" — bare-atom-id
        // keying would crash on the merge.
        window.__sovereign_test__.setHandler(
          "enrich_get_starter_questions",
          ({ corpusId }: { corpusId: string }) => [
            {
              text: `Question from ${corpusId}?`,
              atom_id: "question-0001",
              source_section: null,
              question_type: "thematic",
            },
          ],
        );
      }, 1);
    });

    await bootToChat(page, chat);

    // Both chips must render without a Svelte runtime error. The
    // SVELTE_CONSOLE_FAIL_PATTERNS guard in test-base.ts trips on
    // each_key_duplicate, so a regression here fails the test even
    // if the chips happen to look fine.
    const chips = page.locator(".chip");
    await expect(chips).toHaveCount(2);
  });

  // The user-reported repro path:
  //   1. App starts (no selection).
  //   2. Click an OLD conversation — chat shows its content.
  //   3. Click "New conversation" — chat MUST clear.
  //   4. Click ANY conversation in the sidebar — chat MUST update.
  //
  // The reported failure is that after step 3, the chat stays stuck
  // on the old conversation no matter what is clicked next. Locks
  // down the regression.
  test("user repro: click old, click New, click another, chat updates each time", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedTwoConversations(page);
    await bootToChat(page, chat);

    // Step 2: click an old conversation.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );

    // Step 3: click "New conversation". The fresh UUID won't have
    // a row in the store; default get_conversation throws for
    // unknown ids. Override to throw for the optimistic id so the
    // catch arm fires.
    await page.evaluate(() => {
      const known = new Set(["conv-alpha", "conv-bravo"]);
      window.__sovereign_test__.setHandler(
        "get_conversation",
        ({ conversationId }: { conversationId: string }) => {
          if (known.has(conversationId)) {
            return {
              id: conversationId,
              title:
                conversationId === "conv-alpha"
                  ? "Conversation Alpha"
                  : "Conversation Bravo",
              messages: [
                {
                  id: `${conversationId}-msg`,
                  role: "assistant",
                  content:
                    conversationId === "conv-alpha"
                      ? "ALPHA-CONTENT"
                      : "BRAVO-CONTENT",
                  created_at: 0,
                },
              ],
              created_at: 0,
              updated_at: 0,
            };
          }
          throw new Error(`conversation ${conversationId} not found`);
        },
      );
    });

    await page.locator(".new-btn").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toHaveCount(0);

    // Step 4: click Bravo — chat must show Bravo's content, not
    // Alpha's, not the empty state.
    await page.getByText("Conversation Bravo").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "BRAVO-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "ALPHA-CONTENT",
    );

    // And clicking Alpha back should switch — chat must update.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );
    await expect(page.locator(".sv-ai-msg .sv-prose")).not.toContainText(
      "BRAVO-CONTENT",
    );
  });

  // Repeated 'New conversation' clicks: each one must clear the
  // chat and bind to the freshly-created id. The sidebar accumulates
  // entries (optimistic prepend), and each click should leave the
  // previously-selected one accessible. Pre-existing conversations
  // also need to be reachable after several New clicks.
  test("multiple 'New conversation' clicks each clear the chat", async ({
    sovereignPage: page,
    chat,
  }) => {
    const recordedIds: string[] = [];
    await page.exposeFunction(
      "__recordCreatedId",
      (id: string) => void recordedIds.push(id),
    );
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("create_conversation", () => {
          const id = `conv-new-${Math.random().toString(36).slice(2, 10)}`;
          // Inform the test driver so it can correlate.
          (
            window as unknown as { __recordCreatedId?: (s: string) => void }
          ).__recordCreatedId?.(id);
          return {
            id,
            title: "New conversation",
            created_at: Math.floor(Date.now() / 1000),
          };
        });
        // Default get_conversation throws — perfect for fresh-UUID
        // conversations that haven't been written to the store yet.
      }, 1);
    });

    await bootToChat(page, chat);

    // Click New three times. Each click must end with the chat in
    // the empty state (no bubbles, no streaming indicator).
    for (let i = 0; i < 3; i += 1) {
      await page.locator(".new-btn").click();
      await expect(page.locator(".bubble")).toHaveCount(0);
      await expect(page.locator(".sv-ai-msg")).toHaveCount(0);
      await expect(page.locator(".typing-indicator")).toHaveCount(0);
    }

    // Sidebar should show three optimistic entries.
    await expect(page.locator(".convo-item")).toHaveCount(3);
  });

  // Boot, send a message in the auto-bound conversation, then click
  // a different conversation in the sidebar. After the bound turn
  // completes (or even before), switching must replace the visible
  // surface with the other conversation's history.
  test("after auto-binding a conversation by sending, can still switch away", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedTwoConversations(page);
    await bootToChat(page, chat);

    // From the empty state, send a message — ChatView calls
    // ensureConversation -> create_conversation, then bound. The new
    // id won't be in our seeded list; the shim's default
    // get_conversation throws for it. The CONVERSATION_BOUND path
    // doesn't HYDRATE, so the user message stays on screen.
    await page.locator(".input-area textarea").fill("hello new convo");
    await page.locator(".send-btn").click();
    await expect.poll(() => chat.api.lastStreamStart()).not.toBeNull();
    const start = (await chat.api.lastStreamStart())!;
    await chat.api.streamTokens(start.messageId, ["bound ", "ack"], 0);
    await chat.api.completeMessage(start.messageId, "bound ack");

    await expect(page.locator(".bubble.user .content")).toHaveText(
      "hello new convo",
    );

    // Now switch to Alpha — the chat must replace the bound
    // conversation's bubbles with Alpha's history.
    await page.getByText("Conversation Alpha").click();
    await expect(page.locator(".sv-ai-msg .sv-prose")).toContainText(
      "ALPHA-CONTENT",
    );
    await expect(page.locator(".bubble.user")).toHaveCount(0);
  });
});

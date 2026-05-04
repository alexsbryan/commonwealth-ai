import { test, expect, bootToChat } from "../fixtures/test-base";

// Reading-surface conversation-history flow — verifies that:
//   1. Citing a conversation-history chunk opens the reading surface
//      with the conversation-shaped renderer (role-tagged bubbles +
//      "View conversation" button), not the default book renderer.
//   2. The "View conversation" button bounces back to the chat by
//      switching `selectedConversationId` to the chunk's
//      `conversation.conversation_id`. The reading surface closes.
//
// Mocks `read_get_chunk_neighbors` so the tests don't need a live
// LanceDB index; the response shape mirrors the backend's
// `NeighborWindowDto` exactly.

test.describe("reading surface — conversation-history", () => {
  // Seed an existing conversation in the sidebar so we can verify
  // the "View conversation" jump targets it. Two extra fixtures the
  // shim doesn't ship by default — the in-process Tauri commands
  // for reading.
  async function seedReadingFixtures(
    page: import("@playwright/test").Page,
  ) {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler("list_conversations", () => [
          {
            id: "conv-host",
            title: "Host conversation",
            created_at: 1,
            updated_at: 2,
            message_count: 2,
          },
          {
            id: "conv-target-uuid",
            title: "Schrödinger thread",
            created_at: 1,
            updated_at: 3,
            message_count: 4,
          },
        ]);
        window.__sovereign_test__.setHandler(
          "get_conversation",
          ({ conversationId }: { conversationId: string }) => ({
            id: conversationId,
            title:
              conversationId === "conv-target-uuid"
                ? "Schrödinger thread"
                : "Host conversation",
            messages: [],
            created_at: 0,
            updated_at: 0,
          }),
        );
        // Reading-surface fetch — return a conversation-history
        // chunk with two role-tagged segments.
        window.__sovereign_test__.setHandler(
          "read_get_chunk_neighbors",
          ({ corpusId }: { corpusId: string }) => {
            if (corpusId !== "conversation-history") return null;
            return {
              center: {
                chunk_id: 42,
                corpus_id: "conversation-history",
                content:
                  "[user] What does Schrödinger mean by negative entropy?\n\n" +
                  "[assistant] He frames life as sustaining order by feeding on it.",
                title: null,
                url: null,
                source_doc_id: "conv-target-uuid",
                section_id: null,
                atom_spans: [],
                metadata: {},
                conversation: {
                  conversation_id: "conv-target-uuid",
                  title: "Schrödinger thread",
                  updated_at: Math.floor(Date.now() / 1000),
                  segments: [
                    {
                      role: "user",
                      content:
                        "What does Schrödinger mean by negative entropy?",
                    },
                    {
                      role: "assistant",
                      content:
                        "He frames life as sustaining order by feeding on it.",
                    },
                  ],
                },
              },
              prev: [],
              next: [],
              outbound_url: null,
              ordering: "id_within_source_doc",
            };
          },
        );
      }, 1);
    });
  }

  // Programmatically open a citation against the reading session
  // store. Equivalent to clicking a citation in the chat — keeps
  // the test focused on the reading-surface behavior, not the
  // citation-click wiring (which has its own tests in
  // chat-conversation-routing.spec.ts).
  async function openConversationCitation(
    page: import("@playwright/test").Page,
  ) {
    await page.evaluate(async () => {
      const mod = await import("/src/lib/stores/readingSession.svelte.ts");
      await mod.readingSession.openCitation(
        "conversation-history",
        42,
        "From your question",
      );
    });
  }

  test("conversation chunk renders as role-tagged bubbles, not prose", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedReadingFixtures(page);
    await bootToChat(page, chat);
    // Start on a real conversation so ChatView is mounted.
    await page.getByText("Host conversation").click();
    await openConversationCitation(page);

    // The conversation card header carries the resolved title
    // (from the augmentation backend, mocked here).
    await expect(page.locator(".conv-title")).toHaveText("Schrödinger thread");

    // Two segment bubbles, one user one assistant — NOT a single
    // prose block. The default ChunkRenderer's `.cited-chunk` class
    // must NOT be present.
    await expect(page.locator(".bubble.bubble-user")).toHaveCount(1);
    await expect(page.locator(".bubble.bubble-assistant")).toHaveCount(1);
    await expect(page.locator(".bubble.bubble-user .content")).toHaveText(
      "What does Schrödinger mean by negative entropy?",
    );
    await expect(
      page.locator(".bubble.bubble-assistant .content"),
    ).toHaveText("He frames life as sustaining order by feeding on it.");

    // The default book renderer's marker must be absent — picking the
    // wrong renderer would surface those classes.
    await expect(page.locator(".cited-chunk")).toHaveCount(0);
  });

  test("'View conversation' jumps the chat sidebar and closes the reading surface", async ({
    sovereignPage: page,
    chat,
  }) => {
    await seedReadingFixtures(page);
    await bootToChat(page, chat);
    await page.getByText("Host conversation").click();
    await openConversationCitation(page);

    // Confirm the reading surface is up, sidebar selection is on
    // the host conversation.
    await expect(page.locator(".reading-surface")).toBeVisible();
    await expect(
      page.locator(".convo-item.selected", { hasText: "Host conversation" }),
    ).toBeVisible();

    // Click the jump button.
    await page.locator(".conv-jump-btn").click();

    // Reading surface goes away (closed by the openConversation
    // action in the store).
    await expect(page.locator(".reading-surface")).toHaveCount(0);

    // Sidebar selection moves to the target conversation.
    await expect(
      page.locator(".convo-item.selected", { hasText: "Schrödinger thread" }),
    ).toBeVisible();
  });

  test("non-conversation chunks still render with the default book renderer", async ({
    sovereignPage: page,
    chat,
  }) => {
    await page.addInitScript(() => {
      const wait = setInterval(() => {
        if (!window.__sovereign_test__) return;
        clearInterval(wait);
        window.__sovereign_test__.setHandler(
          "read_get_chunk_neighbors",
          () => ({
            center: {
              chunk_id: 7,
              corpus_id: "brothers_karamazov",
              content: "It was a lovely autumn day…",
              title: "Brothers Karamazov",
              url: null,
              source_doc_id: "bk-doc",
              section_id: "sec_0001",
              atom_spans: [],
              metadata: {},
              // No `conversation` field — default book chunk.
            },
            prev: [],
            next: [],
            outbound_url: null,
            ordering: "id_within_source_doc",
          }),
        );
      }, 1);
    });

    await bootToChat(page, chat);
    await page.evaluate(async () => {
      const mod = await import("/src/lib/stores/readingSession.svelte.ts");
      await mod.readingSession.openCitation(
        "brothers_karamazov",
        7,
        "From your question",
      );
    });

    // Default renderer's prose container shows up; no conversation
    // card / jump button.
    await expect(page.locator(".reading-surface")).toBeVisible();
    await expect(page.locator(".conv-card")).toHaveCount(0);
    await expect(page.locator(".conv-jump-btn")).toHaveCount(0);
  });
});

import { test, expect, bootToChat } from "../fixtures/test-base";

// Inner Work — voice/plumbing regression suite.
//
// Two complementary specs:
//
// 1. **Surface skill tag.** Skill conveyance moved off the client on
//    2026-05-23: the surface no longer toggles peer skills off/on via
//    `toggle_skill`. Instead, entering the witness and summoning
//    lazily creates a conversation stamped with
//    `surfaceSkillId = "inner-work"`, and `Runtime::resolve_active_mode`
//    enforces the witness-path register/exclusivity server-side from
//    that tag. (The old client-side deactivate-peers / restore-on-exit
//    dance — the 2026-05-04 fix for co-active research/knowledge skills
//    polluting the relational register — is gone; the structural
//    surface override is now the single source of truth.) This spec
//    pins the new contract: the conversation the surface creates
//    carries its surface skill id.
//
// 2. **Render hygiene on a paragraph-shape entry.** The mocked
//    Tauri bridge drives a clean witness response into the surface
//    and asserts the rendered DOM contains zero snake_case / SCREAMING
//    _SNAKE identifiers, no `<think>` / `</think>` markers, no
//    third-person reasoning openers. The voice eval (sovereign voice
//    eval --all) is the headline regression gate against polluted
//    *responses*; this spec is the regression gate against the
//    *renderer* developing a habit of mangling clean ones.
//
// Per the bench's H10-journal-corpus-pollution scenario, a clean
// witness response is prose. Anything snake_case_with_underscores
// that finds its way into the rendered surface is a bug.

const SNAKE_CASE_REGEX = /\b[A-Za-z][A-Za-z0-9]*(?:_[A-Za-z0-9]+)+\b/g;

const REASONING_OPENERS = [
  "<think>",
  "</think>",
  "I need to think",
  "I need to search",
  "Let me search",
  "Looking at my retrieved",
  "I should state",
  "I should note",
  "The user is",
  "The user has",
  "The user wants",
  "The user is asking",
];

test.describe("inner work surface — surface skill tag", () => {
  test("summoning a witness tags the conversation with the inner-work surface skill", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Record the surfaceSkillId every create_conversation carries.
    // Installed AFTER bootToChat so the boot-time default conversation
    // isn't counted — we only assert on the one the witness makes.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args?: unknown) => unknown) => void;
        };
        __createdConversations?: Array<{ surfaceSkillId: string | null }>;
      };
      w.__createdConversations = [];
      w.__sovereign_test__.setHandler("create_conversation", (args: unknown) => {
        const a = (args ?? {}) as { surfaceSkillId?: string };
        w.__createdConversations!.push({
          surfaceSkillId: a.surfaceSkillId ?? null,
        });
        return {
          id: `inner-work-conv-${w.__createdConversations!.length}`,
          title: "Inner Work",
          created_at: Math.floor(Date.now() / 1000),
        };
      });
    });

    await page.getByTestId("open-inner-work").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    // Summon a witness. The first summon of the day lazily creates the
    // conversation, stamped with the surface's skill id. That tag (not
    // a client-side skill toggle) is the structural override that
    // routes the turn down the witness path — `resolve_active_mode`
    // reads it server-side.
    await column.fill("Sitting with what's here.");
    await column.press("Meta+Enter");

    await expect
      .poll(
        async () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __createdConversations: Array<{
                    surfaceSkillId: string | null;
                  }>;
                }
              ).__createdConversations,
          ),
        { timeout: 5_000 },
      )
      .toContainEqual({ surfaceSkillId: "inner-work" });
  });
});

test.describe("inner work surface — render hygiene", () => {
  test("clean witness response renders without leaking code identifiers or reasoning markers", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("open-inner-work").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    // Paragraph-shape entry — the actual shape that triggered the
    // 2026-05-04 incident on the live system.
    await column.fill(
      "I've been thinking about ancestors lately. There's a particular grief in noticing I borrowed a future from people who can't tell me whether they meant for me to have it.",
    );
    await column.press("Meta+Enter");

    const start = await expect
      .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
      .not.toBeNull();
    const s = (await chat.api.lastStreamStart())!;

    // Drive a CLEAN witness response. Metadata stamps the runtime
    // register so a future surface-side gate can flag a wrong-register
    // turn without breaking this contract — the test only asserts
    // what arrives at the user.
    await chat.api.completeMessage(
      s.messageId,
      "The borrowed future, that's a sharp image. What does it feel like to set it down for a moment?",
      { register: "Relational", recalled_memories: [] },
    );

    const witness = page.locator(".witness").last();
    await expect(witness).toContainText("The borrowed future");

    // Hygiene: the rendered witness DOM contains zero snake_case
    // identifiers and no reasoning openers. If a future surface
    // change starts surfacing internal markers (status badges,
    // debug overlays, etc.) under the .witness selector, this catches
    // the regression.
    const witnessHtml = await witness.innerHTML();
    const snakeMatches = witnessHtml.match(SNAKE_CASE_REGEX) ?? [];
    expect(snakeMatches).toEqual([]);
    for (const opener of REASONING_OPENERS) {
      expect(
        witnessHtml,
        `witness html should not contain reasoning marker "${opener}"`,
      ).not.toContain(opener);
    }
  });

  // The negative case — a polluted response IS rendered faithfully.
  // We don't filter at the UI layer (filtering is the runtime's job
  // + the bench's regression gate). This test documents the
  // architectural choice: the surface is a renderer, not a censor.
  // If the user's response ever contains a snake_case_id it's
  // because the runtime emitted one — fix the runtime, not the UI.
  test("polluted response renders verbatim — UI is a renderer, not a filter", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("open-inner-work").click();
    const column = page.locator("textarea.column");
    await expect(column).toBeVisible({ timeout: 3_000 });

    await column.fill("Sitting with what's here.");
    await column.press("Meta+Enter");

    const start = await expect
      .poll(async () => chat.api.lastStreamStart(), { timeout: 5_000 })
      .not.toBeNull();
    const s = (await chat.api.lastStreamStart())!;

    // Synthetic pollution — same shape as the 2026-05-04 leak.
    const polluted =
      "Looking at my retrieved sources: make_sep_like_parquet, skeleton_extraction_prompt. None contain that term.";
    await chat.api.completeMessage(s.messageId, polluted, {
      register: "Factual",
    });

    // The renderer is faithful. The bench is what catches polluted
    // text before it ships — this assertion documents the boundary.
    const witness = page.locator(".witness").last();
    await expect(witness).toContainText("make_sep_like_parquet");
  });
});

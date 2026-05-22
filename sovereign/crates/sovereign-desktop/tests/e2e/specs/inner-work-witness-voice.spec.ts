import { test, expect, bootToChat } from "../fixtures/test-base";

// Inner Work — voice/plumbing regression suite.
//
// Two complementary specs:
//
// 1. **Skill exclusivity round-trip.** When the user enters the
//    inner-work surface, every other active skill is deactivated
//    and inner-work is activated. On exit, the snapshot is restored.
//    This guards against the 2026-05-04 failure shape — a co-active
//    research/knowledge skill leaving the relational register
//    without primacy and a planner with `optional = ["knowledge"]`
//    pulling chunks from code corpora into a heartfelt journal
//    response.
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

// Companion to the snapshot/restore test below. Verifies that the
// peer-skill restoration also runs when the user leaves inner-work
// *without* going through the brand-corner mark — clicking the
// NavRail Settings icon jumps view straight from "inner_work" to
// "settings", bypassing the exit-mark. The keep-alive layer in
// App.svelte means onDestroy never fires either; the per-visit
// `active` effect inside InnerWorkSurface is what actually runs the
// restoration in this path.
test.describe("inner work surface — exit via nav-rail", () => {
  test("nav-rail-to-settings (no exit-mark click) still restores prior skills", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args?: unknown) => unknown) => void;
        };
        __toggleLog?: Array<{ id: string; active: boolean }>;
        __activeSkills?: Set<string>;
      };
      w.__toggleLog = [];
      w.__activeSkills = new Set(["research"]);
      w.__sovereign_test__.setHandler("list_skills", () => {
        const active = w.__activeSkills!;
        return [
          {
            id: "research",
            name: "Research",
            description: "",
            trust_level: "user",
            active: active.has("research"),
          },
          {
            id: "inner-work",
            name: "Inner Work",
            description: "",
            trust_level: "user",
            active: active.has("inner-work"),
          },
        ];
      });
      w.__sovereign_test__.setHandler("toggle_skill", (args: unknown) => {
        const a = args as { skillId?: string; active?: boolean };
        const id = String(a.skillId ?? "");
        const isActive = Boolean(a.active);
        w.__toggleLog!.push({ id, active: isActive });
        if (isActive) w.__activeSkills!.add(id);
        else w.__activeSkills!.delete(id);
        return null;
      });
    });

    await page.getByTestId("open-inner-work").click();
    await expect(page.locator(".dateline")).toBeVisible({ timeout: 3_000 });

    // Skip the brand-corner exit — jump straight to settings via the
    // NavRail. This is the path the user takes when they're done
    // writing and want to flip a setting before chatting.
    await page.getByTestId("nav-settings").click();
    await page.locator(".cfg").waitFor();

    // Poll until the restoration sequence appears in the toggle log.
    await expect
      .poll(
        async () =>
          page.evaluate(() => {
            return (
              window as unknown as {
                __toggleLog: Array<{ id: string; active: boolean }>;
              }
            ).__toggleLog;
          }),
        { timeout: 2_000 },
      )
      .toEqual([
        { id: "research", active: false },
        { id: "inner-work", active: true },
        { id: "inner-work", active: false },
        { id: "research", active: true },
      ]);
  });
});

test.describe("inner work surface — skill exclusivity", () => {
  test("entering deactivates other skills and activates inner-work; exit restores", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Seed two pre-active skills + an inactive inner-work. Track
    // every toggle call so we can replay the sequence at the end.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args?: unknown) => unknown) => void;
        };
        __toggleLog?: Array<{ id: string; active: boolean }>;
        __activeSkills?: Set<string>;
      };
      w.__toggleLog = [];
      w.__activeSkills = new Set(["research", "general-helper"]);

      w.__sovereign_test__.setHandler("list_skills", () => {
        const active = w.__activeSkills!;
        return [
          {
            id: "research",
            name: "Research",
            description: "",
            trust_level: "user",
            active: active.has("research"),
          },
          {
            id: "general-helper",
            name: "General Helper",
            description: "",
            trust_level: "user",
            active: active.has("general-helper"),
          },
          {
            id: "inner-work",
            name: "Inner Work",
            description: "",
            trust_level: "user",
            active: active.has("inner-work"),
          },
        ];
      });
      w.__sovereign_test__.setHandler("toggle_skill", (args: unknown) => {
        const a = args as { skillId?: string; active?: boolean };
        const id = String(a.skillId ?? "");
        const isActive = Boolean(a.active);
        w.__toggleLog!.push({ id, active: isActive });
        if (isActive) w.__activeSkills!.add(id);
        else w.__activeSkills!.delete(id);
        return null;
      });
    });

    // Enter the surface. Wait for the dateline so we know onMount
    // has flushed (the snapshot+activate sequence runs synchronously
    // in the awaited block before any further work).
    await page.getByTestId("open-inner-work").click();
    await expect(page.locator(".dateline")).toBeVisible({ timeout: 3_000 });

    // The toggle log should now contain (in order):
    //   - research  → false  (snapshot deactivation)
    //   - general-helper → false  (snapshot deactivation)
    //   - inner-work → true  (skill of this surface)
    // Order between the two deactivations is not pinned — both
    // deactivations come from a for-of over priorActiveSkillIds
    // which preserves the listSkills order, so research goes first.
    const onEntry = await page.evaluate(() => {
      return (
        window as unknown as { __toggleLog: Array<{ id: string; active: boolean }> }
      ).__toggleLog;
    });
    expect(onEntry).toEqual([
      { id: "research", active: false },
      { id: "general-helper", active: false },
      { id: "inner-work", active: true },
    ]);

    // Active set on the daemon side reflects the same exclusivity.
    const activeAfterEntry = await page.evaluate(() => {
      return Array.from(
        (window as unknown as { __activeSkills: Set<string> }).__activeSkills,
      );
    });
    expect(activeAfterEntry).toEqual(["inner-work"]);

    // Exit via the brand mark. The surface's onDestroy fires
    // toggle_skill("inner-work", false), then re-enables every id
    // in the snapshot.
    await page.locator(".exit-mark").click();
    await expect(page.locator(".app-layout")).toBeVisible();

    // Toggles are async fire-and-forget on destroy; poll until the
    // expected sequence appears so we don't race the unmount.
    await expect
      .poll(
        async () =>
          page.evaluate(() => {
            return (
              window as unknown as {
                __toggleLog: Array<{ id: string; active: boolean }>;
              }
            ).__toggleLog.slice(3);
          }),
        { timeout: 2_000 },
      )
      .toEqual([
        { id: "inner-work", active: false },
        { id: "research", active: true },
        { id: "general-helper", active: true },
      ]);

    const activeAfterExit = await page.evaluate(() =>
      Array.from(
        (window as unknown as { __activeSkills: Set<string> }).__activeSkills,
      ),
    );
    expect(activeAfterExit.sort()).toEqual(["general-helper", "research"]);
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

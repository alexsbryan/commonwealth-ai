// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, type Page } from "@playwright/test";

// Agent-drivable verification of the Wrapped mesh-app bundle. The show reads
// ONE precomputed artifact, so the bridge mock is tiny: `wrappedArtifact`
// returns the fixture deck and `readChunk` serves the cited conversations the
// tap-throughs open. Walks the deck (keyboard + dots + skip), checks the
// rhythm heat grid + verbatim excerpt, the cast constellation, the
// unknown-card-type forward-compat rule, absent-section degradation, and the
// fail-closed path. The artifact builder + verbatim audit are unit-tested in
// `sovereign-meshapp::wrapped`.

const HEATMAP: number[][] = Array.from({ length: 7 }, () => Array(24).fill(0));
HEATMAP[0][2] = 12; // Monday 02:00 — the max cell
HEATMAP[4][22] = 3;

const ARTIFACT = {
  schema_version: 4,
  edition: "all-time",
  built_at_unix: 1781000000,
  corpus_id: "conversations-anthropic",
  corpus_last_updated: 1779656421,
  corpus_fingerprint: "fp",
  cards: [
    {
      type: "scale",
      conversations: 576,
      months_active: 14,
      words_total: 1400000,
      words_user: 400000,
      words_assistant: 1000000,
      first_date: "2025-01-04",
      last_date: "2026-03-01",
      derivation: ["conversations = distinct source documents in the index (576)"],
    },
    {
      type: "rhythm",
      heatmap: HEATMAP,
      total_turns: 9421,
      longest_session: {
        conv_uuid: "conv-rabbit",
        title: "Slots deep dive",
        date: "2025-03-09",
        duration_minutes: 247,
        turns: 61,
        chunk_ids: [11, 12],
        excerpt: { chunk_id: 11, text: "Can you explain how llama.cpp slots work?" },
      },
      // The grid ships already shifted onto the reader's clock — the
      // same offset the night-shift card names, so the deck tells one
      // time. The bundle never re-buckets.
      utc_offset_hours: -7,
      derivation: [
        "heatmap = 9421 timestamped turns bucketed by local weekday × hour",
        "its centre placed at 03:00 local ⇒ local clock = UTC-7",
      ],
    },
    {
      type: "recurring",
      threads: [
        {
          conversations: 3,
          span_days: 401,
          askings: [
            { conv_uuid: "c1", date: "2025-01-10", excerpt: { chunk_id: 11, text: "Can you explain how llama.cpp slots work?" } },
            { conv_uuid: "c2", date: "2025-08-02", excerpt: { chunk_id: 12, text: "Planning with Alice from Acme about Berlin." } },
            { conv_uuid: "c3", date: "2026-02-15", excerpt: { chunk_id: 11, text: "Rust context." } },
          ],
        },
      ],
      derivation: ["576 conversation openings compared pairwise by embedding cosine"],
    },
    {
      type: "turn",
      pivots: [
        {
          conv_uuid: "conv-rabbit",
          title: "Slots deep dive",
          date: "2025-03-09",
          seam_index: 4,
          chunk_count: 12,
          cosine: 0.21,
          conv_median: 0.83,
          drop: 0.62,
          before: { chunk_id: 11, text: "Can you explain how llama.cpp slots work?" },
          after: { chunk_id: 12, text: "Planning with Alice from Acme about Berlin." },
        },
      ],
      derivation: ["a seam counts when its cosine falls below its own conversation's median"],
    },
    {
      type: "obsessions",
      quarters: [
        {
          quarter: "2025-Q1",
          topics: [
            { text: "Rust", label: "Work", conversations: 41, distinctiveness: 3.4, sample: { chunk_id: 11, char_start: 0, char_end: 4, text: "Rust" } },
            { text: "Berlin", label: "Location", conversations: 7, distinctiveness: 1.2, sample: { chunk_id: 12, char_start: 0, char_end: 6, text: "Berlin" } },
          ],
        },
      ],
      derivation: ["ranked by z-scored log-odds against the whole-archive baseline, NOT by count"],
    },
    {
      type: "night_shift",
      utc_offset_hours: -7,
      derivation: ["quietest 4h of user turns = UTC 08:00-11:59", "its centre placed at 03:00 local"],
      bands: [
        {
          name: "late night",
          start_hour: 0,
          end_hour: 5,
          mentions: 516,
          topics: [{ text: "Jung", label: "Theme", conversations: 12, distinctiveness: 2.9, sample: { chunk_id: 11, char_start: 0, char_end: 4, text: "Rust" } }],
        },
        {
          name: "morning",
          start_hour: 6,
          end_hour: 11,
          mentions: 4650,
          topics: [{ text: "Plyometrics", label: "Theme", conversations: 9, distinctiveness: 2.1, sample: { chunk_id: 12, char_start: 0, char_end: 6, text: "Berlin" } }],
        },
      ],
    },
    {
      type: "cast",
      nodes: [
        { id: "alice", canonical_name: "Alice", entity_type: "Person", degree: 1, bridging: 0.9, conversations: 12, first_date: "2025-01-04", last_date: "2026-02-01", sample: { chunk_id: 12, char_start: 0, char_end: 5, text: "Alice" } },
        { id: "acme", canonical_name: "Acme", entity_type: "Organization", degree: 1, bridging: 0.4, conversations: 9, first_date: "2025-02-04", last_date: "2025-12-01", sample: { chunk_id: 12, char_start: 0, char_end: 4, text: "Acme" } },
        { id: "anna karenina", canonical_name: "Anna Karenina", entity_type: "Work", degree: 0, bridging: 0.0, conversations: 3, first_date: "2025-05-04", last_date: "2025-06-01", sample: { chunk_id: 11, char_start: 0, char_end: 13, text: "Anna Karenina" } },
      ],
      edges: [{ source: "alice", target: "acme", co_conversations: 5, pmi: 1.31, first_date: "2025-02-04", last_date: "2025-11-20" }],
      derivation: ["node size is betweenness, not frequency"],
    },
    { type: "door" },
  ],
};

/** Index of each card in the fixture deck — the dots follow this order. */
const CARD = { scale: 0, rhythm: 1, recurring: 2, turn: 3, obsessions: 4, night_shift: 5, cast: 6, door: 7 };

const CHUNKS: Record<string, string> = {
  "11": "### [2025-03-09 15:00] user\n\nCan you explain how llama.cpp slots work? Rust context. Anna Karenina aside.\n\n### [2025-03-09 15:02] assistant\n\nSlots are lazy-loaded model instances.",
  "12": "### [2025-04-01 10:00] user\n\nPlanning with Alice from Acme about Berlin.\n\n### [2025-04-01 10:01] assistant\n\nNoted.",
};

async function installBridge(page: Page, artifact: unknown = ARTIFACT) {
  await page.addInitScript(
    (data) => {
      const { artifact, chunks } = data as any;
      (window as any).meshApp = {
        capabilities: async () => ({ mesh_store_read: true, mesh_store_write: false, inference_access: false, knowledge_access: false }),
        wrappedArtifact: async () => artifact,
        readChunk: async (_c: string, chunkId: string) => ({ chunk_id: String(chunkId), content: chunks[String(chunkId)] ?? "", title: null }),
        openOuterWork: async (corpusId: string) => {
          (window as any).__openedOuterWork = corpusId;
        },
      };
    },
    { artifact, chunks: CHUNKS },
  );
}

const dots = (page: Page) => page.locator(".story-dots .story-dot");
const currentDot = (page: Page) => page.locator('.story-dot[aria-current="true"]');
const activeSlide = (page: Page) => page.locator(".story-slide.active");

test.describe("Wrapped mesh app bundle", () => {
  test("the deck opens on the scale card with host-computed counts", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await expect(page.locator("#loading")).toBeHidden();
    await expect(dots(page)).toHaveCount(8);
    const slide = activeSlide(page);
    await expect(slide).toContainText("576");
    await expect(slide).toContainText("14 months");
    await expect(slide).toContainText("1,400,000 words");
    await expect(slide).toContainText("Anna Karenina, 4 times over");
    await expect(slide).toContainText("computed on this machine");
  });

  test("arrow keys and dots navigate; the active dot tracks", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await expect(currentDot(page)).toHaveCount(1);
    await page.keyboard.press("ArrowRight");
    await expect(activeSlide(page)).toHaveAttribute("data-card", "rhythm");
    await page.keyboard.press("ArrowLeft");
    await expect(activeSlide(page)).toHaveAttribute("data-card", "scale");
    // Dots jump directly.
    await dots(page).nth(CARD.door).click();
    await expect(activeSlide(page)).toHaveAttribute("data-card", "door");
    await expect(activeSlide(page)).toContainText("Your archive is now your memory");
  });

  test("the door's call-to-action asks the host to open Outer Work, scoped", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.door).click();
    await activeSlide(page).getByRole("button", { name: /ask your past self/i }).click();
    await expect
      .poll(async () => page.evaluate(() => (window as any).__openedOuterWork))
      .toBe("conversations-anthropic");
  });

  test("the door degrades to copy when the host can't navigate", async ({ page }) => {
    await installBridge(page);
    await page.addInitScript(() => {
      // Override after the base mock: dev-server behavior.
      const m = (window as any).meshApp;
      m.openOuterWork = async () => { throw new Error("no chat to open"); };
    });
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.door).click();
    await activeSlide(page).getByRole("button", { name: /ask your past self/i }).click();
    await expect(activeSlide(page)).toContainText("Open this in the desktop app");
  });

  test("skip advances and marks the dot; the door card has no skip", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await activeSlide(page).getByRole("button", { name: "Skip" }).click();
    await expect(activeSlide(page)).toHaveAttribute("data-card", "rhythm");
    await expect(dots(page).nth(0)).toHaveClass(/skipped/);
    await dots(page).nth(CARD.door).click();
    await expect(activeSlide(page).getByRole("button", { name: "Skip" })).toHaveCount(0);
  });

  test("the rhythm card renders the heat grid and the verbatim rabbit-hole", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await page.keyboard.press("ArrowRight");
    const slide = activeSlide(page);
    // The kicker + headline survive the grid render — heatGrid() clears
    // whatever container it is given, and once got the whole slide.
    await expect(slide).toContainText("When you think");
    await expect(slide).toContainText("9,421 turns");
    await expect(slide.locator(".heat-cell")).toHaveCount(7 * 24);
    const max = slide.locator('.heat-cell[data-i="4"]');
    await expect(max).toHaveCount(1);
    await expect(max).toHaveAttribute("title", "Mon 02:00 — 12");
    // The longest session: 247 min, 61 turns, the user's own opening words.
    await expect(slide).toContainText("4 hours 7 minutes");
    await expect(slide).toContainText("61 turns on 2025-03-09");
    await expect(slide.locator(".excerpt")).toHaveText("Can you explain how llama.cpp slots work?");
    // Tap-through dereferences to the cited conversation text.
    await slide.getByRole("button", { name: /where it went/ }).first().click();
    await expect(slide.locator(".card-full").first()).toContainText("Slots are lazy-loaded");
    // One clock for the whole deck: the grid is local, and says so.
    await expect(slide).toContainText("on your own clock");
    await slide.getByRole("button", { name: /why this/ }).first().click();
    await expect(slide).toContainText("local clock = UTC-7");
  });

  test("the obsessions card counts conversations and cites a sample", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.obsessions).click();
    const slide = activeSlide(page);
    await expect(slide).toContainText("2025-Q1");
    await expect(slide).toContainText("Rust");
    await expect(slide).toContainText("41 conversations");
    await slide.getByRole("button", { name: /one of them/ }).first().click();
    await expect(slide.locator(".card-full").first()).toContainText("llama.cpp slots");
  });

  test("the cast constellation renders nodes and a clicked node cites itself", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.cast).click();
    const slide = activeSlide(page);
    await expect(slide.locator("svg circle")).toHaveCount(3);
    await expect(slide.locator("svg")).toContainText("Alice");
    await slide.locator("svg circle").first().click();
    await expect(slide).toContainText(/conversations,/);
    // Edges carry their evidence now, not a hard-coded "appears_with".
    await expect(slide).toContainText("5 shared conversations");
    await slide.getByRole("button", { name: /where they appear/ }).click();
    await expect(slide.locator(".card-full")).toContainText("Alice from Acme");
  });

  test("the recurring card leads with the SPAN, and every asking is verbatim", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.recurring).click();
    const slide = activeSlide(page);
    // 401 days must read as a span a person would say, not a raw count.
    await expect(slide).toContainText("You came back to this 3 times over 13 months");
    await expect(slide.locator(".excerpt")).toHaveCount(3);
    await expect(slide).toContainText("2025-01-10");
    await expect(slide).toContainText("2026-02-15");
    await slide.getByRole("button", { name: /that conversation/ }).first().click();
    await expect(slide.locator(".card-full").first()).toContainText("Slots are lazy-loaded");
  });

  test("the turn card quotes both sides of the seam", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.turn).click();
    const slide = activeSlide(page);
    await expect(slide).toContainText("On 2025-03-09");
    await expect(slide).toContainText("Slots deep dive");
    await expect(slide).toContainText("before");
    await expect(slide).toContainText("after");
    const quotes = slide.locator(".excerpt");
    await expect(quotes.nth(0)).toHaveText("Can you explain how llama.cpp slots work?");
    await expect(quotes.nth(1)).toHaveText("Planning with Alice from Acme about Berlin.");
  });

  test("the night shift states the LOCAL clock it is claiming in", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(CARD.night_shift).click();
    const slide = activeSlide(page);
    await expect(slide).toContainText("late night");
    await expect(slide).toContainText("00:00–05:59");
    await expect(slide).toContainText("Jung");
    await expect(slide).toContainText("Plyometrics");
    // The offset is on the card, not buried — the claim is false without it.
    await expect(slide).toContainText("your local clock (UTC-7)");
  });

  test("every claim card exposes the host's derivation behind 'why this?'", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    for (const card of ["recurring", "turn", "obsessions", "night_shift", "cast"] as const) {
      await dots(page).nth(CARD[card]).click();
      const slide = activeSlide(page);
      const why = slide.getByRole("button", { name: /why this/ });
      await expect(why).toHaveCount(1);
      await why.click();
      await expect(slide.locator("ul.meta")).toBeVisible();
    }
  });

  test("unknown card types are skipped — the enriched-deck forward-compat seam", async ({ page }) => {
    const future = {
      ...ARTIFACT,
      cards: [
        ARTIFACT.cards[CARD.scale],
        { type: "archetype", name: "The Night Debugger" }, // not in this bundle's renderers
        ARTIFACT.cards[CARD.door],
      ],
    };
    await installBridge(page, future);
    await page.goto("/meshapp/wrapped/index.html");
    await expect(dots(page)).toHaveCount(2); // scale + door, archetype skipped
    await page.keyboard.press("ArrowRight");
    await expect(activeSlide(page)).toHaveAttribute("data-card", "door");
  });

  test("absent sections shrink the deck — absent data means an absent card", async ({ page }) => {
    const sparse = { ...ARTIFACT, cards: [ARTIFACT.cards[CARD.scale], ARTIFACT.cards[CARD.rhythm], ARTIFACT.cards[CARD.door]] };
    await installBridge(page, sparse);
    await page.goto("/meshapp/wrapped/index.html");
    await expect(dots(page)).toHaveCount(3);
  });

  test("a bridge denial fails closed — error banner, no show", async ({ page }) => {
    await page.addInitScript(() => {
      (window as any).meshApp = {
        capabilities: async () => ({ mesh_store_read: false, mesh_store_write: false, inference_access: false, knowledge_access: false }),
        wrappedArtifact: async () => { throw new Error("permission denied: mesh_store_read"); },
      };
    });
    await page.goto("/meshapp/wrapped/index.html");
    await expect(page.locator("#error")).toBeVisible();
    await expect(page.locator("#error")).toContainText(/denied|failed|Couldn't/i);
    await expect(page.locator(".story-show")).toHaveCount(0);
  });

  test("the real host shim wires wrappedArtifact to the IPC primitive", async ({ page }) => {
    await page.addInitScript(
      (data) => {
        const { artifact } = data as any;
        (window as any).__TAURI_INTERNALS__ = {
          invoke: async (cmd: string) => {
            if (cmd === "meshapp_wrapped_artifact") return artifact;
            if (cmd === "meshapp_read_chunk") return { chunk_id: "0", content: "", title: null };
            return null;
          },
        };
      },
      { artifact: ARTIFACT },
    );
    await page.addInitScript({ path: "src-tauri/src/meshapp_shim.js" });
    await page.goto("/meshapp/wrapped/index.html");
    await expect(dots(page)).toHaveCount(8);
    await expect(activeSlide(page)).toContainText("576");
  });
});

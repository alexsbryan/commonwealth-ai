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
  schema_version: 1,
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
    },
    {
      type: "obsessions",
      quarters: [
        {
          quarter: "2025-Q1",
          topics: [
            { text: "Rust", label: "Work", conversations: 41, sample: { chunk_id: 11, char_start: 0, char_end: 4, text: "Rust" } },
            { text: "Berlin", label: "Location", conversations: 7, sample: { chunk_id: 12, char_start: 0, char_end: 6, text: "Berlin" } },
          ],
        },
      ],
    },
    {
      type: "cast",
      nodes: [
        { id: "alice", canonical_name: "Alice", entity_type: "Person", degree: 1, conversations: 12, sample: { chunk_id: 12, char_start: 0, char_end: 5, text: "Alice" } },
        { id: "acme", canonical_name: "Acme", entity_type: "Organization", degree: 1, conversations: 9, sample: { chunk_id: 12, char_start: 0, char_end: 4, text: "Acme" } },
        { id: "anna karenina", canonical_name: "Anna Karenina", entity_type: "Work", degree: 0, conversations: 3, sample: { chunk_id: 11, char_start: 0, char_end: 13, text: "Anna Karenina" } },
      ],
      edges: [{ source: "alice", target: "acme", relationship_type: "appears_with", co_conversations: 5 }],
    },
    { type: "door" },
  ],
};

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
    await expect(dots(page)).toHaveCount(5);
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
    await dots(page).nth(4).click();
    await expect(activeSlide(page)).toHaveAttribute("data-card", "door");
    await expect(activeSlide(page)).toContainText("Your archive is now your memory");
  });

  test("the door's call-to-action asks the host to open Outer Work, scoped", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(4).click();
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
    await dots(page).nth(4).click();
    await activeSlide(page).getByRole("button", { name: /ask your past self/i }).click();
    await expect(activeSlide(page)).toContainText("Open this in the desktop app");
  });

  test("skip advances and marks the dot; the door card has no skip", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await activeSlide(page).getByRole("button", { name: "Skip" }).click();
    await expect(activeSlide(page)).toHaveAttribute("data-card", "rhythm");
    await expect(dots(page).nth(0)).toHaveClass(/skipped/);
    await dots(page).nth(4).click();
    await expect(activeSlide(page).getByRole("button", { name: "Skip" })).toHaveCount(0);
  });

  test("the rhythm card renders the heat grid and the verbatim rabbit-hole", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await page.keyboard.press("ArrowRight");
    const slide = activeSlide(page);
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
  });

  test("the obsessions card counts conversations and cites a sample", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/wrapped/index.html");
    await dots(page).nth(2).click();
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
    await dots(page).nth(3).click();
    const slide = activeSlide(page);
    await expect(slide.locator("svg circle")).toHaveCount(3);
    await expect(slide.locator("svg")).toContainText("Alice");
    await slide.locator("svg circle").first().click();
    await expect(slide).toContainText(/conversations\./);
    await slide.getByRole("button", { name: /where they appear/ }).click();
    await expect(slide.locator(".card-full")).toContainText("Alice from Acme");
  });

  test("unknown card types are skipped — the enriched-deck forward-compat seam", async ({ page }) => {
    const future = {
      ...ARTIFACT,
      cards: [
        ARTIFACT.cards[0],
        { type: "archetype", name: "The Night Debugger" }, // not in this bundle's renderers
        ARTIFACT.cards[4],
      ],
    };
    await installBridge(page, future);
    await page.goto("/meshapp/wrapped/index.html");
    await expect(dots(page)).toHaveCount(2); // scale + door, archetype skipped
    await page.keyboard.press("ArrowRight");
    await expect(activeSlide(page)).toHaveAttribute("data-card", "door");
  });

  test("absent sections shrink the deck — absent data means an absent card", async ({ page }) => {
    const sparse = { ...ARTIFACT, cards: [ARTIFACT.cards[0], ARTIFACT.cards[1], ARTIFACT.cards[4]] };
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
    await expect(dots(page)).toHaveCount(5);
    await expect(activeSlide(page)).toContainText("576");
  });
});

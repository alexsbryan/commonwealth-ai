// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, type Page } from "@playwright/test";

// Agent-drivable verification of the Project Blue Book mesh-app bundle. We
// serve the static bundle (vite serves `public/` at root) and MOCK the host
// bridge `window.meshApp` with the exact shapes the investigation-graph ops
// return (findings / searchEntities / node), so the bundle's rendering +
// graph navigation are verified headlessly. The Rust bridge + its
// fail-closed authorization are unit-tested separately (`meshapp.rs`).

const HOTSPOTS = [
  {
    pattern_name: "sighting_hotspots",
    pattern_kind: "threshold",
    entities: [{ id: "inst-wpafb", canonical_name: "Wright-Patterson AFB, Ohio", entity_type: "installation" }],
    attributes: { value: 15, attribute: "sighting_count", threshold: 3 },
  },
  {
    pattern_name: "sighting_hotspots",
    pattern_kind: "threshold",
    entities: [{ id: "inst-dallas", canonical_name: "Dallas, Pennsylvania", entity_type: "installation" }],
    attributes: { value: 6, attribute: "sighting_count", threshold: 3 },
  },
];

// A two-hop graph: installation → (occurred_near) sighting → (resolved_as)
// adjudication, each edge quoting its Form-10073 card.
const NODES: Record<string, unknown> = {
  "inst-wpafb": {
    id: "inst-wpafb",
    canonical_name: "Wright-Patterson AFB, Ohio",
    entity_type: "installation",
    attributes: { branch: "USAF" },
    aliases: ["WPAFB", "Wright-Patterson Air Forca Base"],
    edges: [
      {
        relationship_type: "occurred_near",
        direction: "in",
        other_id: "s-1",
        other_name: "0 OCT 56",
        other_type: "sighting",
        excerpt: "object observed near the base perimeter, radar-confirmed",
        source_chunk: "BB-955-p3",
        confidence: 0.9,
        attributes: {},
      },
    ],
  },
  "s-1": {
    id: "s-1",
    canonical_name: "0 OCT 56",
    entity_type: "sighting",
    attributes: { time_of_day: "night" },
    aliases: [],
    edges: [
      {
        relationship_type: "officially_resolved_as",
        direction: "out",
        other_id: "adj-1",
        other_name: "UNIDENTIFIED",
        other_type: "adjudication",
        excerpt: "conclusion: UNIDENTIFIED",
        source_chunk: "BB-955-p1",
        confidence: 1,
        attributes: {},
      },
    ],
  },
};

// Full OCR'd card narratives behind the edges' source_chunk ids — the rich
// content a citation expands into (vs. the short edge excerpt).
const CARDS: Record<string, string> = {
  "BB-955-p3":
    "OBJECT SIGHTED 18 JUL 54 ... SHAPED LIKE DISC. SIZE OF GRAPEFRUIT. " +
    "YELLOWISH GOLD STAR BRIGHTNESS ... MOVED STEADILY S OR SW AT 30,000 TO " +
    "40,000 FT ... 3 TO 5 MIN ... ST LOUIS ILL, CIV.",
  "BB-955-p1":
    "29 March 53 · Spooner, Wisconsin · SOURCE: Civilians · CONCLUSION: " +
    "UNIDENTIFIED · Circular aluminum-colored object approx 1/2 size of moon.",
};

type Node = { entity_type: string; canonical_name: string; id: string; aliases: string[]; edges: unknown[] };

async function installBridge(page: Page) {
  await page.addInitScript(
    (data) => {
      const { hotspots, nodes, cards } = data as {
        hotspots: unknown[];
        nodes: Record<string, Node>;
        cards: Record<string, string>;
      };
      (window as unknown as { meshApp: unknown }).meshApp = {
        capabilities: async () => ({
          mesh_store_read: true,
          mesh_store_write: false,
          inference_access: false,
          knowledge_access: false,
        }),
        findings: async (_c: string, pattern?: string) =>
          pattern === "sighting_hotspots" ? hotspots : [],
        searchEntities: async (_c: string, q: string, nodeType?: string) => {
          const ql = String(q).toLowerCase();
          return Object.values(nodes)
            .filter((n) => !nodeType || n.entity_type === nodeType)
            .filter((n) => n.canonical_name.toLowerCase().includes(ql))
            .map((n) => ({
              id: n.id,
              canonical_name: n.canonical_name,
              entity_type: n.entity_type,
              degree: n.edges.length,
              alias_count: n.aliases.length,
              attributes: {},
            }));
        },
        node: async (_c: string, id: string) =>
          nodes[id] ?? { id, canonical_name: id, entity_type: "?", attributes: {}, aliases: [], edges: [] },
        readChunk: async (_c: string, chunkId: string) => ({
          chunk_id: String(chunkId),
          content: cards[String(chunkId)] ?? "",
          title: null,
        }),
      };
    },
    { hotspots: HOTSPOTS, nodes: NODES, cards: CARDS },
  );
}

test.describe("Project Blue Book mesh app bundle", () => {
  test("renders the hotspot ranking from findings (sorted by count)", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    const hots = page.locator("#hotspots .hot");
    await expect(hots).toHaveCount(2);
    await expect(hots.first()).toContainText("Wright-Patterson AFB, Ohio"); // 15 > 6
    await expect(hots.first()).toContainText("15 unexplained");
    await expect(page.locator("#loading")).toBeHidden();
    await expect(page.locator("#error")).toBeHidden();
  });

  test("drilling into a hotspot shows cited evidence (excerpt + card chunk)", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    await page.locator("#hotspots .hot", { hasText: "Wright-Patterson" }).click();
    await expect(page.locator("#detail")).toBeVisible();
    await expect(page.locator("#d-name")).toHaveText("Wright-Patterson AFB, Ohio");
    await expect(page.locator("#d-type")).toHaveText("installation");
    await expect(page.locator("#d-meta")).toContainText("folded OCR variant"); // the coalescing
    // The cited edge: relationship + the quoted card text + the chunk id.
    const edge = page.locator("#edges .edge").first();
    await expect(edge).toContainText("occurred_near");
    await expect(edge.locator(".excerpt")).toContainText("radar-confirmed");
    await expect(edge.locator(".prov")).toContainText("BB-955-p3");
    // An installation (the HQ) spans many cards → no single primary auto-card.
    await expect(page.locator("#primary-card")).toBeEmpty();
  });

  test("a case/sighting auto-surfaces its primary Form-10073 card", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    await page.locator("#hotspots .hot", { hasText: "Wright-Patterson" }).click();
    await page.locator("#edges .edge .link", { hasText: "0 OCT 56" }).click(); // → sighting s-1
    await expect(page.locator("#d-type")).toHaveText("sighting");
    // Shown automatically — no click — for a narrative entity.
    await expect(page.locator("#primary-card")).toContainText("Form-10073");
    await expect(page.locator("#primary-card .card-full")).toContainText("UNIDENTIFIED");
  });

  test("expanding a citation reveals the full OCR'd card narrative", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    await page.locator("#hotspots .hot", { hasText: "Wright-Patterson" }).click();
    const edge = page.locator("#edges .edge").first();
    // The card is collapsed until asked — the bundle shows only the excerpt.
    await expect(edge.locator(".card-full")).toBeHidden();
    await edge.getByRole("button", { name: /read the full card/i }).click();
    // ... then the whole Form-10073 narrative (object, altitude, witness).
    await expect(edge.locator(".card-full")).toBeVisible();
    await expect(edge.locator(".card-full")).toContainText("SHAPED LIKE DISC");
    await expect(edge.locator(".card-full")).toContainText("ST LOUIS ILL");
  });

  test("navigating an edge traverses the graph to the next entity", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    await page.locator("#hotspots .hot", { hasText: "Wright-Patterson" }).click();
    // Click the sighting on the far end of the edge → drill into it.
    await page.locator("#edges .edge .link", { hasText: "0 OCT 56" }).click();
    await expect(page.locator("#d-name")).toHaveText("0 OCT 56");
    await expect(page.locator("#d-type")).toHaveText("sighting");
    // The Air Force's own disposition, cited to its card.
    await expect(page.locator("#edges")).toContainText("officially_resolved_as");
    await expect(page.locator("#edges")).toContainText("UNIDENTIFIED");
    await expect(page.locator("#edges .prov").first()).toContainText("BB-955-p1");
  });

  test("search finds an installation and loads it", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/uap/index.html");

    await page.getByRole("textbox", { name: /search installations/i }).fill("Wright");
    await page.getByRole("button", { name: /^search$/i }).click();
    await page.locator("#matches .match", { hasText: "Wright-Patterson" }).click();
    await expect(page.locator("#d-name")).toHaveText("Wright-Patterson AFB, Ohio");
  });

  test("a bridge denial fails closed — error banner, no graph", async ({ page }) => {
    await page.addInitScript(() => {
      (window as unknown as { meshApp: unknown }).meshApp = {
        capabilities: async () => ({
          mesh_store_read: false,
          mesh_store_write: false,
          inference_access: false,
          knowledge_access: false,
        }),
        findings: async () => {
          throw new Error("permission denied: mesh_store_read");
        },
      };
    });
    await page.goto("/meshapp/uap/index.html");
    await expect(page.locator("#error")).toBeVisible();
    await expect(page.locator("#error")).toContainText(/denied|failed/i);
    await expect(page.locator("#app")).toBeHidden();
  });

  test("the real host shim wires window.meshApp.findings to the IPC primitive", async ({ page }) => {
    // Mock the always-present IPC primitive, then inject the REAL shim over
    // it — exercising the shim→invoke path the mocked specs skip (where the
    // withGlobalTauri-off bug once hid).
    await page.addInitScript(
      (data) => {
        const { hotspots, nodes } = data as { hotspots: unknown[]; nodes: Record<string, unknown> };
        (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
          invoke: async (cmd: string, args: { id?: string }) => {
            if (cmd === "meshapp_findings") return hotspots;
            if (cmd === "meshapp_node") return nodes[args.id ?? ""];
            if (cmd === "meshapp_search_entities") return [];
            return null;
          },
        };
      },
      { hotspots: HOTSPOTS, nodes: NODES },
    );
    await page.addInitScript({ path: "src-tauri/src/meshapp_shim.js" });
    await page.goto("/meshapp/uap/index.html");

    await expect(page.locator("#hotspots .hot")).toHaveCount(2);
    await expect(page.locator("#hotspots .hot").first()).toContainText("Wright-Patterson AFB, Ohio");
  });
});

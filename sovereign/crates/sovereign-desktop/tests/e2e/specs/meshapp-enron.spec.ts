import { test, expect, type Page } from "@playwright/test";

// Agent-drivable verification of the Enron mesh-app bundle. We serve the
// static bundle (vite serves `public/` at root) and MOCK the host bridge
// `window.meshApp` with the exact shapes the ATLAS-backed ops return
// (corpusStats / subgraph / timeline / reconciliation / searchEntities / node /
// readChunk), so the story-first experience — scale banner, guided on-ramp,
// force-graph, collapse timeline, reconciliation reveal, cited drill-down — is
// verified headlessly. The Rust adapter + ops are unit-tested in `meshapp.rs`.

const STATS = {
  atoms: 6101, entities: 1730, events: 461, states: 782, relations: 651,
  claims: 800, questions: 1677, edges: 3302, reconciled_merges: 35, documents: 3722,
};

const node = (id: string, name: string, type: string, degree: number, desc: string) => ({
  id, canonical_name: name, entity_type: type, degree, alias_count: 0,
  attributes: { description: desc },
});

const GRAPH: Record<string, { nodes: unknown[]; edges: unknown[] }> = {
  institution: {
    nodes: [
      node("e-enron", "Enron", "institution", 213, "The company at the center."),
      node("e-elpaso", "El Paso", "institution", 52, "Texas energy and pipeline company."),
      node("e-dynegy", "Dynegy", "institution", 27, "The rival that proposed to rescue Enron."),
    ],
    edges: [
      { source: "e-enron", target: "e-elpaso", relationship_type: "counterparty_of" },
      { source: "e-enron", target: "e-dynegy", relationship_type: "counterparty_of" },
    ],
  },
  person: {
    nodes: [
      node("e-lay", "Kenneth Lay", "person", 236, "Former leader of Enron."),
      node("e-skilling", "Jeff Skilling", "person", 102, "Former Enron CEO who left with millions."),
    ],
    edges: [{ source: "e-lay", target: "e-skilling", relationship_type: "colleague_of" }],
  },
};

const RECON = [
  { canonical_id: "e-calpine", canonical_name: "Calpine Corporation",
    surface_forms: ["Calpine", "Calpine Corp.", "Calpine Corporation"],
    signals_fired: ["name_similarity"], source_count: 3 },
  { canonical_id: "e-elpaso", canonical_name: "El Paso",
    surface_forms: ["El Paso", "El Paso Corp."], signals_fired: ["name_similarity"], source_count: 2 },
];

const TIMELINE = {
  buckets: [
    { ym: "2001-04", count: 5, chunk_ids: [10, 11] },
    { ym: "2001-08", count: 9, chunk_ids: [20] },
    { ym: "2001-11", count: 22, chunk_ids: [30, 31] },
    { ym: "2001-12", count: 14, chunk_ids: [40] },
  ],
  dated: 50, total: 60,
};

const NODES: Record<string, unknown> = {
  "e-lay": {
    id: "e-lay", canonical_name: "Kenneth Lay", entity_type: "person",
    attributes: { description: "Former leader of Enron." }, aliases: ["Ken Lay", "K. Lay"],
    edges: [{
      relationship_type: "counterparty_of", direction: "out", other_id: "e-dynegy",
      other_name: "Dynegy", other_type: "institution", excerpt: "Lay described the Dynegy rescue",
      source_chunk: "30", confidence: 1, attributes: {},
    }],
  },
  "e-elpaso": {
    id: "e-elpaso", canonical_name: "El Paso", entity_type: "institution",
    attributes: {
      description: "Texas energy and pipeline company.",
      reconciliation: { surface_forms: ["El Paso", "El Paso Corp."], signals_fired: ["name_similarity"], source_count: 2 },
    },
    aliases: ["El Paso Corp."],
    edges: [{
      relationship_type: "counterparty_of", direction: "in", other_id: "e-enron",
      other_name: "Enron", other_type: "institution", excerpt: "El Paso and Enron discussed the asset sale",
      source_chunk: "200", confidence: 1, attributes: {},
    }],
  },
  "e-calpine": {
    id: "e-calpine", canonical_name: "Calpine Corporation", entity_type: "institution",
    attributes: {
      description: "Independent power producer.",
      reconciliation: { surface_forms: ["Calpine", "Calpine Corp.", "Calpine Corporation"], signals_fired: ["name_similarity"], source_count: 3 },
    },
    aliases: ["Calpine"], edges: [],
  },
  "e-dynegy": {
    id: "e-dynegy", canonical_name: "Dynegy", entity_type: "institution",
    attributes: { description: "The rival that proposed to rescue Enron." }, aliases: [], edges: [],
  },
  "e-fastow": {
    id: "e-fastow", canonical_name: "Andy Fastow", entity_type: "person",
    attributes: { description: "Whose personal financial involvement is questioned." },
    aliases: ["Andrew Fastow"], edges: [],
  },
};

const SEARCHABLE = [
  node("e-lay", "Kenneth Lay", "person", 236, "Former leader of Enron."),
  node("e-skilling", "Jeff Skilling", "person", 102, "Former Enron CEO who left with millions."),
  node("e-fastow", "Andy Fastow", "person", 96, "Whose personal financial involvement is questioned."),
  node("e-dynegy", "Dynegy", "institution", 27, "The rival that proposed to rescue Enron."),
  node("e-elpaso", "El Paso", "institution", 52, "Texas energy and pipeline company."),
  node("e-calpine", "Calpine Corporation", "institution", 10, "Independent power producer."),
];

const EMAILS: Record<string, string> = {
  "30": "From: klay@enron.com\nTo: all.employees@enron.com\nDate: Fri, 09 Nov 2001\nSubject: Dynegy\n\nWe have agreed to merge with Dynegy.",
  "200": "From: bod@elpaso.com\nTo: klay@enron.com\nSubject: Assets\n\nFollowing up on the pipeline assets.",
  "10": "From: a@enron.com\nDate: Apr 2001\nSubject: spring\n\nbody",
};

async function installBridge(page: Page) {
  await page.addInitScript(
    (data) => {
      const { stats, graph, recon, timeline, nodes, searchable, emails } = data as any;
      (window as any).meshApp = {
        capabilities: async () => ({ mesh_store_read: true, mesh_store_write: false, inference_access: false, knowledge_access: false }),
        corpusStats: async () => stats,
        subgraph: async (_c: string, nodeType?: string | null) =>
          nodeType ? (graph[nodeType] ?? { nodes: [], edges: [] })
                   : { nodes: [...graph.institution.nodes, ...graph.person.nodes],
                       edges: [...graph.institution.edges, ...graph.person.edges] },
        timeline: async () => timeline,
        reconciliation: async () => recon,
        searchEntities: async (_c: string, q: string, nodeType?: string | null) => {
          const ql = String(q).toLowerCase();
          return searchable
            .filter((n: any) => !nodeType || n.entity_type === nodeType)
            .filter((n: any) => n.canonical_name.toLowerCase().includes(ql));
        },
        node: async (_c: string, id: string) =>
          nodes[id] ?? { id, canonical_name: id, entity_type: "?", attributes: {}, aliases: [], edges: [] },
        readChunk: async (_c: string, chunkId: string) => ({ chunk_id: String(chunkId), content: emails[String(chunkId)] ?? "", title: null }),
      };
    },
    { stats: STATS, graph: GRAPH, recon: RECON, timeline: TIMELINE, nodes: NODES, searchable: SEARCHABLE, emails: EMAILS },
  );
}

test.describe("Enron mesh app bundle", () => {
  test("the scale banner conveys the machine-built provenance", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    await expect(page.locator("#loading")).toBeHidden();
    const banner = page.locator("#banner");
    await expect(banner).toContainText("3,722"); // emails
    await expect(banner).toContainText("1,730"); // entities
    await expect(banner).toContainText("humans read them");
    await expect(banner).toContainText("0");
  });

  test("the on-ramp leads with machine-written descriptions", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    const threads = page.locator("#threads .thread");
    await expect(threads.filter({ hasText: "Kenneth Lay" })).toBeVisible();
    await expect(page.locator("#threads")).toContainText("Former leader of Enron");
    await expect(page.locator("#threads")).toContainText("Andy Fastow"); // resolved from "Fastow"
    // Clicking a thread opens the cited drill-down.
    await threads.filter({ hasText: "Kenneth Lay" }).click();
    await expect(page.locator("#d-name")).toHaveText("Kenneth Lay");
    await expect(page.locator("#d-desc")).toContainText("Former leader of Enron");
  });

  test("the force-graph renders the companies with labels", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    const svg = page.locator("#map svg");
    await expect(svg.locator("circle")).toHaveCount(3);
    await expect(svg.locator("text", { hasText: "Enron" })).toBeVisible();
    await expect(page.locator("#map-msg")).toContainText("3 nodes");
  });

  test("the node-type toggle switches the graph to people", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    await page.getByRole("button", { name: /^people$/i }).click();
    await expect(page.locator("#map svg")).toContainText("Kenneth Lay");
    await expect(page.locator("#map svg circle")).toHaveCount(2);
  });

  test("the collapse timeline renders monthly bars and drills into a month", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    const cols = page.locator("#timeline .tl-col");
    await expect(cols).toHaveCount(4);
    await expect(page.locator("#timeline")).toContainText("Nov '01");
    // Click the spike → that month's emails appear, expandable to the source.
    await cols.filter({ hasText: "Nov '01" }).click();
    await expect(page.locator("#timeline-detail")).toContainText("22 emails in Nov '01");
    await page.locator("#timeline-detail").getByRole("button", { name: /read email/i }).first().click();
    await expect(page.locator("#timeline-detail .card-full").first()).toContainText("merge with Dynegy");
  });

  test("a reconciliation row reveals its folded forms, and opens the entity", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    const merges = page.locator("#merges .merge");
    await expect(merges).toHaveCount(2);
    const calpine = merges.filter({ hasText: "Calpine Corporation" });
    await expect(calpine.locator(".chip.signal")).toContainText("matching name");
    // Clicking the row plays the reveal.
    await calpine.click();
    await expect(calpine.locator(".reveal")).toHaveClass(/on/);
    // Clicking the canonical name opens the entity (with its merge provenance).
    await calpine.locator(".canon").click();
    await expect(page.locator("#d-name")).toHaveText("Calpine Corporation");
    await expect(page.locator("#d-recon")).toContainText("3 different names");
  });

  test("drilling into a company shows reconciliation provenance + cited email", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    await page.getByRole("textbox", { name: /search entities/i }).fill("El Paso");
    await page.getByRole("button", { name: /^search$/i }).click();
    await page.locator("#matches .match", { hasText: "El Paso" }).click();
    await expect(page.locator("#d-recon")).toContainText("One identity");
    const edge = page.locator("#edges .edge").first();
    await expect(edge).toContainText("counterparty_of");
    await edge.getByRole("button", { name: /read the source email/i }).click();
    await expect(edge.locator(".card-full")).toContainText("pipeline assets");
  });

  test("search surfaces descriptions and loads an entity", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");
    await page.getByRole("textbox", { name: /search entities/i }).fill("Fastow");
    await page.getByRole("button", { name: /^search$/i }).click();
    const match = page.locator("#matches .match", { hasText: "Andy Fastow" });
    await expect(match).toContainText("personal financial involvement");
    await match.click();
    await expect(page.locator("#d-name")).toHaveText("Andy Fastow");
  });

  test("a bridge denial fails closed — error banner, no app", async ({ page }) => {
    await page.addInitScript(() => {
      (window as any).meshApp = {
        capabilities: async () => ({ mesh_store_read: false, mesh_store_write: false, inference_access: false, knowledge_access: false }),
        subgraph: async () => { throw new Error("permission denied: mesh_store_read"); },
      };
    });
    await page.goto("/meshapp/enron/index.html");
    await expect(page.locator("#error")).toBeVisible();
    await expect(page.locator("#error")).toContainText(/denied|failed/i);
    await expect(page.locator("#app")).toBeHidden();
  });

  test("the real host shim wires the new ops to the IPC primitive", async ({ page }) => {
    // Mock the always-present IPC primitive, inject the REAL shim over it —
    // exercising the shim→invoke path for the new corpusStats/subgraph/
    // timeline/reconciliation methods the mocked specs skip.
    await page.addInitScript(
      (data) => {
        const { stats, graph, recon, timeline } = data as any;
        (window as any).__TAURI_INTERNALS__ = {
          invoke: async (cmd: string, args: { nodeType?: string | null }) => {
            if (cmd === "meshapp_subgraph") return args.nodeType ? graph[args.nodeType] : graph.institution;
            if (cmd === "meshapp_corpus_stats") return stats;
            if (cmd === "meshapp_timeline") return timeline;
            if (cmd === "meshapp_reconciliation") return recon;
            if (cmd === "meshapp_search_entities") return [];
            return null;
          },
        };
      },
      { stats: STATS, graph: GRAPH, recon: RECON, timeline: TIMELINE },
    );
    await page.addInitScript({ path: "src-tauri/src/meshapp_shim.js" });
    await page.goto("/meshapp/enron/index.html");

    await expect(page.locator("#banner")).toContainText("3,722");
    await expect(page.locator("#map svg circle")).toHaveCount(3);
    await expect(page.locator("#timeline .tl-col")).toHaveCount(4);
    await expect(page.locator("#merges .merge")).toHaveCount(2);
  });
});

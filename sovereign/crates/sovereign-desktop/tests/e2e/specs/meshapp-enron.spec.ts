import { test, expect, type Page } from "@playwright/test";

// Agent-drivable verification of the Enron mesh-app bundle. We serve the
// static bundle (vite serves `public/` at root) and MOCK the host bridge
// `window.meshApp` with the exact shapes the ATLAS-backed ops return (graph /
// reconciliation / searchEntities / node / readChunk), so the bundle's three
// views — counterparty centrality, the reconciliation glassbox, and the cited
// drill-down — are verified headlessly. The Rust adapter (atlas→graph) and its
// fail-closed authorization are unit-tested separately (`meshapp.rs`).

// Degree-ranked entities per node type — Enron the hub, then the real
// counterparties; people are the exec cast.
const GRAPH: Record<string, unknown[]> = {
  institution: [
    { id: "e-enron", canonical_name: "Enron", entity_type: "institution", degree: 213, alias_count: 134, attributes: {} },
    { id: "e-elpaso", canonical_name: "El Paso", entity_type: "institution", degree: 52, alias_count: 54, attributes: {} },
    { id: "e-dynegy", canonical_name: "Dynegy", entity_type: "institution", degree: 27, alias_count: 4, attributes: {} },
  ],
  person: [
    { id: "e-lay", canonical_name: "Kenneth Lay", entity_type: "person", degree: 236, alias_count: 28, attributes: {} },
    { id: "e-skilling", canonical_name: "Jeff Skilling", entity_type: "person", degree: 102, alias_count: 16, attributes: {} },
  ],
};

// The cross-inbox merge log: canonical + folded surface forms + the signal.
const RECON = [
  {
    canonical_id: "e-calpine",
    canonical_name: "Calpine Corporation",
    surface_forms: ["Calpine", "Calpine Corp.", "Calpine Corporation"],
    signals_fired: ["name_similarity"],
    source_count: 3,
  },
  {
    canonical_id: "e-elpaso",
    canonical_name: "El Paso",
    surface_forms: ["El Paso", "El Paso Corp."],
    signals_fired: ["name_similarity"],
    source_count: 2,
  },
];

// Drill-down nodes. `attributes.reconciliation` is present on the canonicals
// that were merges; an event edge carries `attributes.description` as a label.
const NODES: Record<string, unknown> = {
  "e-elpaso": {
    id: "e-elpaso",
    canonical_name: "El Paso",
    entity_type: "institution",
    attributes: {
      description: "Texas energy and pipeline company.",
      reconciliation: { surface_forms: ["El Paso", "El Paso Corp."], signals_fired: ["name_similarity"], source_count: 2 },
    },
    aliases: ["El Paso Corp.", "PGET"],
    edges: [
      {
        relationship_type: "counterparty_of",
        direction: "in",
        other_id: "e-enron",
        other_name: "Enron",
        other_type: "institution",
        excerpt: "El Paso and Enron discussed the pipeline asset sale",
        source_chunk: "200",
        confidence: 1,
        attributes: {},
      },
    ],
  },
  "e-enron": {
    id: "e-enron",
    canonical_name: "Enron",
    entity_type: "institution",
    attributes: { description: "The company." },
    aliases: ["Enron Corp.", "the company"],
    edges: [
      {
        relationship_type: "counterparty_of",
        direction: "out",
        other_id: "e-elpaso",
        other_name: "El Paso",
        other_type: "institution",
        excerpt: "Enron to acquire El Paso pipeline capacity",
        source_chunk: "201",
        confidence: 0.95,
        attributes: {},
      },
    ],
  },
  "e-calpine": {
    id: "e-calpine",
    canonical_name: "Calpine Corporation",
    entity_type: "institution",
    attributes: {
      description: "Independent power producer.",
      reconciliation: {
        surface_forms: ["Calpine", "Calpine Corp.", "Calpine Corporation"],
        signals_fired: ["name_similarity"],
        source_count: 3,
      },
    },
    aliases: ["Calpine"],
    edges: [
      {
        relationship_type: "unspecified",
        direction: "in",
        other_id: "e-lay",
        other_name: "Kenneth Lay",
        other_type: "person",
        excerpt: "Date: Thu, 26 Jul 2001",
        source_chunk: "100",
        confidence: 1,
        attributes: { description: "Lay emailed Calpine about the partnership" },
      },
    ],
  },
};

// Full source-email bodies behind the edges' source_chunk ids.
const EMAILS: Record<string, string> = {
  "200":
    "From: bod@elpaso.com\nTo: klay@enron.com\nSubject: Pipeline assets\n\n" +
    "Ken — following up on the El Paso pipeline capacity we discussed last week.",
  "201": "From: klay@enron.com\nTo: traders@enron.com\nSubject: El Paso deal\n\nWe should move on the capacity.",
  "100": "Date: Thu, 26 Jul 2001\nFrom: klay@enron.com\nTo: deals@calpine.com\n\nLet's talk partnership.",
};

type Node = { entity_type: string; canonical_name: string; id: string; aliases: string[]; edges: unknown[] };

async function installBridge(page: Page) {
  await page.addInitScript(
    (data) => {
      const { graph, recon, nodes, emails } = data as {
        graph: Record<string, Node[]>;
        recon: unknown[];
        nodes: Record<string, Node>;
        emails: Record<string, string>;
      };
      (window as unknown as { meshApp: unknown }).meshApp = {
        capabilities: async () => ({
          mesh_store_read: true,
          mesh_store_write: false,
          inference_access: false,
          knowledge_access: false,
        }),
        graph: async (_c: string, nodeType?: string | null) => {
          if (!nodeType) return [...graph.institution, ...graph.person];
          return graph[nodeType] ?? [];
        },
        reconciliation: async () => recon,
        searchEntities: async (_c: string, q: string, nodeType?: string | null) => {
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
          content: emails[String(chunkId)] ?? "",
          title: null,
        }),
      };
    },
    { graph: GRAPH, recon: RECON, nodes: NODES, emails: EMAILS },
  );
}

test.describe("Enron mesh app bundle", () => {
  test("renders counterparty centrality (companies by degree)", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    const rows = page.locator("#graph .hot");
    await expect(rows).toHaveCount(3);
    await expect(rows.first()).toContainText("Enron"); // 213 highest
    await expect(rows.first()).toContainText("213 links");
    await expect(page.locator("#loading")).toBeHidden();
    await expect(page.locator("#error")).toBeHidden();
  });

  test("the node-type toggle switches the graph to people", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.getByRole("button", { name: /^people$/i }).click();
    const rows = page.locator("#graph .hot");
    await expect(rows.first()).toContainText("Kenneth Lay"); // 236
    await expect(rows.first()).toContainText("236 links");
  });

  test("renders the reconciliation merge log with its reasons", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    const merges = page.locator("#merges .merge");
    await expect(merges).toHaveCount(2);
    // Richest merge first (3 surface forms), with its signal as a chip.
    await expect(merges.first()).toContainText("Calpine Corporation");
    await expect(merges.first()).toContainText("Calpine Corp.");
    await expect(merges.first().locator(".chip.signal")).toContainText("name_similarity");
  });

  test("drilling into a company shows its reconciliation provenance + cited edge", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.locator("#graph .hot", { hasText: "El Paso" }).click();
    await expect(page.locator("#detail")).toBeVisible();
    await expect(page.locator("#d-name")).toHaveText("El Paso");
    await expect(page.locator("#d-type")).toHaveText("institution");
    // The glassbox: why this canonical exists + the folded forms.
    await expect(page.locator("#d-recon")).toContainText("Reconciled identity");
    await expect(page.locator("#d-recon")).toContainText("El Paso Corp.");
    // The cited edge: relationship + quoted email text + chunk id.
    const edge = page.locator("#edges .edge").first();
    await expect(edge).toContainText("counterparty_of");
    await expect(edge.locator(".excerpt")).toContainText("pipeline asset sale");
    await expect(edge.locator(".prov")).toContainText("200");
  });

  test("expanding a citation reveals the full source email", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.locator("#graph .hot", { hasText: "El Paso" }).click();
    const edge = page.locator("#edges .edge").first();
    await expect(edge.locator(".card-full")).toBeHidden();
    await edge.getByRole("button", { name: /read the source email/i }).click();
    await expect(edge.locator(".card-full")).toBeVisible();
    await expect(edge.locator(".card-full")).toContainText("klay@enron.com");
    await expect(edge.locator(".card-full")).toContainText("pipeline capacity");
  });

  test("navigating an edge traverses the graph to the next entity", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.locator("#graph .hot", { hasText: "El Paso" }).click();
    await page.locator("#edges .edge .link", { hasText: "Enron" }).click();
    await expect(page.locator("#d-name")).toHaveText("Enron");
    await expect(page.locator("#edges")).toContainText("counterparty_of");
  });

  test("an event edge shows its LLM description label", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    // Calpine's cited edge is an Event carrying a description.
    await page.locator("#merges .merge", { hasText: "Calpine" }).click();
    await expect(page.locator("#d-name")).toHaveText("Calpine Corporation");
    await expect(page.locator("#edges .edge").first()).toContainText("Lay emailed Calpine about the partnership");
  });

  test("clicking a reconciliation merge opens the canonical entity", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.locator("#merges .merge", { hasText: "Calpine" }).click();
    await expect(page.locator("#d-name")).toHaveText("Calpine Corporation");
    await expect(page.locator("#d-recon")).toContainText("3 surface forms");
  });

  test("search finds an entity and loads it", async ({ page }) => {
    await installBridge(page);
    await page.goto("/meshapp/enron/index.html");

    await page.getByRole("textbox", { name: /search entities/i }).fill("Calpine");
    await page.getByRole("button", { name: /^search$/i }).click();
    await page.locator("#matches .match", { hasText: "Calpine" }).click();
    await expect(page.locator("#d-name")).toHaveText("Calpine Corporation");
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
        graph: async () => {
          throw new Error("permission denied: mesh_store_read");
        },
      };
    });
    await page.goto("/meshapp/enron/index.html");
    await expect(page.locator("#error")).toBeVisible();
    await expect(page.locator("#error")).toContainText(/denied|failed/i);
    await expect(page.locator("#app")).toBeHidden();
  });

  test("the real host shim wires graph + reconciliation to the IPC primitive", async ({ page }) => {
    // Mock the always-present IPC primitive, then inject the REAL shim over it
    // — exercising the shim→invoke path (incl. the new meshapp_reconciliation
    // method) that the mocked specs skip.
    await page.addInitScript(
      (data) => {
        const { graph, recon } = data as { graph: Record<string, unknown[]>; recon: unknown[] };
        (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
          invoke: async (cmd: string, args: { nodeType?: string | null }) => {
            if (cmd === "meshapp_graph") return args.nodeType ? graph[args.nodeType] ?? [] : graph.institution;
            if (cmd === "meshapp_reconciliation") return recon;
            if (cmd === "meshapp_search_entities") return [];
            return null;
          },
        };
      },
      { graph: GRAPH, recon: RECON },
    );
    await page.addInitScript({ path: "src-tauri/src/meshapp_shim.js" });
    await page.goto("/meshapp/enron/index.html");

    await expect(page.locator("#graph .hot")).toHaveCount(3);
    await expect(page.locator("#graph .hot").first()).toContainText("Enron");
    await expect(page.locator("#merges .merge")).toHaveCount(2);
    await expect(page.locator("#merges .merge").first()).toContainText("Calpine Corporation");
  });
});

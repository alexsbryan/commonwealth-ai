// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// Activity & Sharing surface (rebuilt SharingSection). Pins the
// frontend contract: given the daemon's activity reads, the totals
// card, the "given to the mesh" slice, the live feed, and the reins
// controls render without crashing. The Tauri command surface is
// mocked via the shim — this is a render/robustness test, not a real
// daemon test. (The Members render path is covered by mesh-health.)

interface ActivityFixture {
  // Daemon-side activity summary (/internal/activity/summary).
  summary: Record<string, unknown>;
  // In-process chat slice (summarize_chat_activity).
  chat: Record<string, unknown>;
  // Unified recent feed (/internal/activity/recent).
  recent: Array<{ timestamp: number; kind: Record<string, unknown> }>;
}

/** Prime every load-path command SharingSection.refresh() fans out.
 *  Must run AFTER bootToChat (which resets the shim on goto("/")). */
async function primeActivity(page: Page, f: ActivityFixture): Promise<void> {
  await page.evaluate((fx) => {
    const t = window.__sovereign_test__;
    t.setHandler("get_activity_summary", () => fx.summary);
    t.setHandler("get_chat_activity", () => fx.chat);
    t.setHandler("get_activity_recent", () => fx.recent);
    t.setHandler("get_contribution_status", () => ({
      ceiling: 2,
      in_flight: 0,
      paused_until: null,
      pause_remaining_secs: null,
      yield_peers_to_foreground: true,
      yielding_secs_remaining: null,
    }));
    t.setHandler("get_recent_contributions", () => []);
    t.setHandler("lc_newsworthy_status", () => ({
      last_tick: null,
      local_corpus_installed: false,
      leader_node_id: null,
      installed_peer_count: 0,
      self_in_pool: false,
    }));
    t.setHandler("get_ingest_budget", () => ({ throttle_factor: 0.5 }));
    t.setHandler("get_mesh_quiesced", () => ({ quiesced: false }));
  }, f);
}

async function openActivityTab(
  page: Page,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  chat: any,
  f: ActivityFixture,
): Promise<void> {
  await bootToChat(page, chat);
  await primeActivity(page, f);
  await page.getByTestId("nav-settings").click();
  await page
    .locator(".cfg-toc .toc-item")
    .filter({ hasText: /^Activity & Sharing$/ })
    .click();
  await page.locator(".sharing").waitFor();
}

function emptyEmbeddings() {
  return { local_requests: 0, peer_requests: 0, local_units: 0, peer_units: 0 };
}

function baseSummary(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    window_days: 7,
    local_inference_requests: 0,
    local_tokens_generated: 0,
    local_inference_wall_seconds: 0,
    embeddings: emptyEmbeddings(),
    local_knowledge_queries: 0,
    local_chunks_served: 0,
    corpora: [],
    total_chunks_ingested: 0,
    newsworthy_fetches: 0,
    newsworthy_articles: 0,
    peer_inference_served_requests: 0,
    peer_inference_served_tokens: 0,
    peer_knowledge_queries_served: 0,
    peer_bytes_served: 0,
    peer_bytes_received: 0,
    ...over,
  };
}

function baseChat(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    window_days: 7,
    turns: 0,
    tokens_generated: 0,
    chunks_retrieved: 0,
    by_corpus: [],
    by_model: [],
    ...over,
  };
}

test.describe("Activity & Sharing: totals, feed, controls", () => {
  test("totals card renders local usage in Sovereign vocabulary", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openActivityTab(page, chat, {
      summary: baseSummary({
        total_chunks_ingested: 3021,
        embeddings: { ...emptyEmbeddings(), local_units: 512 },
        corpora: [
          {
            corpus_id: "obsidian-vault",
            chunks_ingested: 3021,
            ingest_runs: 2,
            ingest_seconds: 610,
            enrich_runs: 1,
            enrich_atoms: 450,
            enrich_seconds: 900,
          },
        ],
      }),
      chat: baseChat({ turns: 37, tokens_generated: 14200, chunks_retrieved: 88 }),
      recent: [],
    });

    const totals = page.locator(".sharing .totals-grid").first();
    await expect(totals).toBeVisible();
    // Numbers render compact: 14_200 → "14.2k", 3021 → "3k"; sub-1000
    // counts stay exact (37, 88).
    await expect(totals).toContainText("14.2k");
    await expect(totals).toContainText("tokens generated");
    await expect(totals).toContainText("37");
    await expect(totals).toContainText("questions answered");
    await expect(totals).toContainText("88");
    await expect(totals).toContainText("chunks retrieved");
    await expect(totals).toContainText("3k");
    await expect(totals).toContainText("chunks ingested");
    // Per-corpus ingest breakdown surfaces the corpus by name.
    await expect(page.locator(".sharing .corpus-list")).toContainText(
      "obsidian-vault",
    );
    // With no peer activity, the mesh slice stays hidden.
    await expect(page.locator(".sharing")).not.toContainText("Given to the mesh");
  });

  test("large totals render compact (k / M), never raw grouped digits", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openActivityTab(page, chat, {
      summary: baseSummary({ total_chunks_ingested: 10_000 }),
      chat: baseChat({
        turns: 999,
        tokens_generated: 1_500_000,
        chunks_retrieved: 250_000,
      }),
      recent: [],
    });
    const totals = page.locator(".sharing .totals-grid").first();
    await expect(totals).toContainText("1.5M"); // tokens generated
    await expect(totals).toContainText("250k"); // chunks retrieved
    await expect(totals).toContainText("10k"); // chunks ingested
    await expect(totals).toContainText("999"); // sub-1000 stays exact
    // The compact form replaces the long grouped form entirely.
    await expect(totals).not.toContainText("1,500,000");
    await expect(totals).not.toContainText("250,000");
  });

  test("'given to the mesh' slice renders when this node served peers", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openActivityTab(page, chat, {
      summary: baseSummary({
        embeddings: { ...emptyEmbeddings(), peer_requests: 4, peer_units: 256 },
        peer_inference_served_requests: 12,
        peer_inference_served_tokens: 4800,
        peer_knowledge_queries_served: 5,
        peer_bytes_served: 5_000_000_000,
      }),
      chat: baseChat(),
      recent: [],
    });
    const sharing = page.locator(".sharing");
    await expect(sharing).toContainText("Given to the mesh");
    await expect(sharing).toContainText("inferences served");
    await expect(sharing).toContainText("12");
  });

  test("recent feed summarises both local and peer events", async ({
    sovereignPage: page,
    chat,
  }) => {
    const now = Math.floor(Date.now() / 1000);
    await openActivityTab(page, chat, {
      summary: baseSummary({ total_chunks_ingested: 3021 }),
      chat: baseChat(),
      recent: [
        {
          timestamp: now - 30,
          kind: { type: "ChunksIngested", corpus_id: "obsidian-vault", chunks: 3021, duration_secs: 600 },
        },
        {
          timestamp: now - 90,
          kind: { type: "EmbeddingsServed", served_for: { actor: "peer" }, n_texts: 64, tokens: 4096 },
        },
      ],
    });
    const feed = page.locator(".sharing .feed");
    await expect(feed).toBeVisible();
    await expect(feed).toContainText("Ingested");
    await expect(feed).toContainText("obsidian-vault");
    await expect(feed).toContainText("Embedded");
  });

  test("the reins: background-work throttle presets render and reflect current factor", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openActivityTab(page, chat, {
      summary: baseSummary(),
      chat: baseChat(),
      recent: [],
    });
    // The controls section renders the throttle presets; "Balanced"
    // (0.5) is marked active because get_ingest_budget returned 0.5.
    const reins = page.locator(".sharing");
    await expect(reins).toContainText("The reins");
    await expect(reins).toContainText("Background work");
    const balanced = reins.locator("button.preset", { hasText: "Balanced" });
    await expect(balanced).toHaveClass(/active/);
    // Mesh-participation toggle is present.
    await expect(reins).toContainText("Stop participating in shared work");
  });
});

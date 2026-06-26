// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// CorpusFilterStrip — the toggle-chip surface that lets users
// scope retrieval to a subset of installed corpora per-conversation.
// The chip strip lives in two places (empty state + above the input
// area), reading/writing the same `enabled_corpora` row column via
// the `set_conversation_enabled_corpora` Tauri command.
//
// These tests exercise the UI ↔ Tauri contract:
//   • toggling a chip persists via set_conversation_enabled_corpora
//   • the payload normalizes "all enabled" → null (sentinel)
//   • Send is gated when every parent has been muted

// Two-parent fixture: wikipedia + sep. Layer corpora (parent set)
// are filtered out by the strip itself so they shouldn't render
// as chips. The fixture exercises that filter too.
const INSTALLED_CORPORA = [
  {
    id: "wikipedia",
    name: "Wikipedia",
    description: "",
    size_compressed_gb: 0,
    size_indexed_gb: 0,
    license: "CC-BY-SA",
    tiers: [],
    status: "installed",
    chunks_count: 1000,
    enrichment_enabled: false,
    indexed_at: 1,
    embedding_model: "qwen-embedding-0.6b",
    embedding_dimensions: 1024,
    vector_index_ready: true,
    parent_corpus_id: null,
  },
  {
    id: "sep",
    name: "SEP",
    description: "",
    size_compressed_gb: 0,
    size_indexed_gb: 0,
    license: "free-for-research",
    tiers: [],
    status: "installed",
    chunks_count: 500,
    enrichment_enabled: true,
    indexed_at: 1,
    embedding_model: "qwen-embedding-0.6b",
    embedding_dimensions: 1024,
    vector_index_ready: true,
    parent_corpus_id: null,
  },
  // Layer corpus: parent_corpus_id != null — should NOT render as
  // its own chip. Retrieval expands the parent's allow-list to
  // include this layer automatically; the UI hides it.
  {
    id: "wikipedia-newsworthy",
    name: "Wikipedia Newsworthy",
    description: "",
    size_compressed_gb: 0,
    size_indexed_gb: 0,
    license: "CC-BY-SA",
    tiers: [],
    status: "installed",
    chunks_count: 50,
    enrichment_enabled: false,
    indexed_at: 1,
    embedding_model: "qwen-embedding-0.6b",
    embedding_dimensions: 1024,
    vector_index_ready: true,
    parent_corpus_id: "wikipedia",
  },
];

test.describe("corpus filter strip", () => {
  test.beforeEach(async ({ sovereignPage: page }) => {
    // Install both fixtures BEFORE the page mounts so the strip's
    // onMount listCorpora() call resolves to the fixture set, not
    // the default empty array.
    await page.addInitScript((corpora) => {
      const apply = () => {
        const api = (window as unknown as { __sovereign_test__?: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        } }).__sovereign_test__;
        if (!api) {
          // Shim not installed yet — try again on next tick.
          setTimeout(apply, 0);
          return;
        }
        api.setHandler("list_corpora", () => corpora);
      };
      apply();
    }, INSTALLED_CORPORA);
  });

  test("renders one chip per parent corpus, hides layers", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    // Move 1: the strip now lives behind the AskScopeBar — reveal it.
    await page.getByTestId("ask-scope-bar").click();

    const strip = page.locator(".corpus-filter-strip").first();
    await expect(strip).toBeVisible();

    // Exactly two chips — Wikipedia + SEP. The layer corpus
    // (wikipedia-newsworthy) follows the parent at retrieval time
    // and must not get its own toggle.
    const chips = strip.locator(".kb-tag");
    await expect(chips).toHaveCount(2);
    await expect(chips.nth(0)).toContainText("Wikipedia");
    await expect(chips.nth(1)).toContainText("SEP");
  });

  test("toggling a chip persists the allow-list subset", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    // Move 1: the strip now lives behind the AskScopeBar — reveal it.
    await page.getByTestId("ask-scope-bar").click();

    // The strip lives in two mount points — the empty-state copy
    // and the persistent above-input copy. Both share the same
    // selection state; we drive the empty-state one because the
    // user sees it first and that's where first-touch ergonomics
    // matter most.
    const wikiChip = page
      .locator(".corpus-filter-strip")
      .first()
      .locator(".kb-tag", { hasText: "Wikipedia" });

    // Pre-toggle: chip should render as enabled (no .disabled class).
    await expect(wikiChip).not.toHaveClass(/disabled/);
    await wikiChip.click();
    await expect(wikiChip).toHaveClass(/disabled/);

    // The shim records the most recent payload sent to
    // set_conversation_enabled_corpora. Toggle off Wikipedia
    // leaves SEP as the only enabled parent — so the persisted
    // allow-list is exactly ["sep"].
    await expect
      .poll(
        async () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __sovereign_test__: {
                    _lastEnabledCorpora: {
                      conversationId: string;
                      enabledCorpora: string[] | null;
                    } | null;
                  };
                }
              ).__sovereign_test__._lastEnabledCorpora,
          ),
        { timeout: 2_000 },
      )
      .toMatchObject({ enabledCorpora: ["sep"] });
  });

  test("re-enabling the last muted chip normalizes to null", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    // Move 1: the strip now lives behind the AskScopeBar — reveal it.
    await page.getByTestId("ask-scope-bar").click();

    const strip = page.locator(".corpus-filter-strip").first();
    const wikiChip = strip.locator(".kb-tag", { hasText: "Wikipedia" });

    // Mute Wikipedia → enabled_corpora = ["sep"].
    await wikiChip.click();
    await expect
      .poll(async () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __sovereign_test__: {
                  _lastEnabledCorpora: {
                    enabledCorpora: string[] | null;
                  } | null;
                };
              }
            ).__sovereign_test__._lastEnabledCorpora?.enabledCorpora,
        ),
      )
      .toEqual(["sep"]);

    // Re-enable → every parent is back in the set, so the strip
    // normalizes to the "no filter" sentinel (`null`). This keeps
    // newly-installed corpora opt-in by default for the
    // conversation. See nextSelection() in CorpusFilterStrip.svelte.
    await wikiChip.click();
    await expect
      .poll(async () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __sovereign_test__: {
                  _lastEnabledCorpora: {
                    enabledCorpora: string[] | null;
                  } | null;
                };
              }
            ).__sovereign_test__._lastEnabledCorpora?.enabledCorpora,
        ),
      )
      .toBeNull();
  });

  test("Send is disabled when every parent has been muted", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    // Move 1: the strip now lives behind the AskScopeBar — reveal it.
    await page.getByTestId("ask-scope-bar").click();

    const strip = page.locator(".corpus-filter-strip").first();
    const chips = strip.locator(".kb-tag");

    // Mute both chips.
    await chips.nth(0).click();
    await chips.nth(1).click();
    await expect(chips.nth(0)).toHaveClass(/disabled/);
    await expect(chips.nth(1)).toHaveClass(/disabled/);

    // Typing a message should NOT enable Send while every source is
    // muted. The hint surfaces below the input row.
    await page.locator(".input-area textarea").fill("hello sovereign");
    await expect(page.locator(".send-btn")).toBeDisabled();
    await expect(
      page.locator(".oversize-hint", {
        hasText: "Enable at least one source",
      }),
    ).toBeVisible();
  });
});

// ─── Outer-Work deep link (Wrapped Door card) ────────────────────────
// The `meshapp-open-outer-work` host event must land the user on a
// fresh conversation whose strip shows ONLY the relevant conversations
// corpus selected — and persist that allow-list before the first send.
// Regression: the strip hydrates exactly once per conversation-id flip,
// so the scope must be in `initialEnabled` BEFORE the row is minted.

const CONV_CORPUS = {
  id: "conversations-anthropic",
  name: "Claude conversations",
  description: "",
  size_compressed_gb: 0,
  size_indexed_gb: 0,
  license: "private",
  tiers: [],
  status: "installed",
  chunks_count: 600,
  enrichment_enabled: true,
  indexed_at: 1,
  embedding_model: "qwen-embedding-0.6b",
  embedding_dimensions: 1024,
  vector_index_ready: true,
  parent_corpus_id: null,
};

test.describe("outer-work deep link scopes the strip", () => {
  test.beforeEach(async ({ sovereignPage: page }) => {
    await page.addInitScript(
      (corpora) => {
        const apply = () => {
          const api = (window as unknown as { __sovereign_test__?: {
            setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
          } }).__sovereign_test__;
          if (!api) {
            setTimeout(apply, 0);
            return;
          }
          api.setHandler("list_corpora", () => corpora);
        };
        apply();
      },
      [...INSTALLED_CORPORA, CONV_CORPUS],
    );
  });

  test("the event selects only the conversations corpus and persists it", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    // Move 1: the strip now lives behind the AskScopeBar — reveal it.
    await page.getByTestId("ask-scope-bar").click();

    // Fire the host event a mesh-app Door card triggers.
    await page.evaluate(() => {
      (window as unknown as {
        __sovereign_test__: { emit: (e: string, p: unknown) => number };
      }).__sovereign_test__.emit("meshapp-open-outer-work", {
        corpus_id: "conversations-anthropic",
      });
    });

    // The strip shows the conversations chip ENABLED and every other
    // parent muted — the visible promise of "ask your past self".
    const strip = page.locator(".corpus-filter-strip").first();
    const convChip = strip.locator(".kb-tag", { hasText: "Claude conversations" });
    await expect(convChip).not.toHaveClass(/disabled/);
    await expect(strip.locator(".kb-tag", { hasText: "Wikipedia" })).toHaveClass(/disabled/);
    await expect(strip.locator(".kb-tag", { hasText: "SEP" })).toHaveClass(/disabled/);

    // …and the allow-list persisted on the freshly-minted row.
    await expect
      .poll(
        async () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __sovereign_test__: {
                    _lastEnabledCorpora: {
                      enabledCorpora: string[] | null;
                    } | null;
                  };
                }
              ).__sovereign_test__._lastEnabledCorpora?.enabledCorpora,
          ),
        { timeout: 2_000 },
      )
      .toEqual(["conversations-anthropic"]);
  });
});

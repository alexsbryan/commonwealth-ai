// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";

// Recipe Author workspace — happy-path coverage for M2. Uses the
// mocked Tauri shim so the workspace renders without a real daemon.
//
// What the spec proves:
// - The "Recipe Author" entry on the left nav rail (gated behind
//   the `enable_recipe_authoring` config flag) opens the workspace.
// - "+ New project" creates a project, refreshes the sidebar, auto-
//   selects, and renders all the dashboard cards.
// - "← Back to chat" returns to the chat workspace.
//
// 2026-05-25 navigation rewrite: the entry moved off the chat
// sidebar (testid `open-recipe-author`, now gone) onto the left
// NavRail as `nav-recipe-author`, surfaced only when App.svelte
// reads `enable_recipe_authoring: true` from get_config. The shim
// default returns true, so the entry shows by default; the
// "hidden when flag off" case overrides get_config to false.
//
// Pre-2026-05-24 this spec also asserted that workspace enter/exit
// toggled the recipe-author skill via
// `recipe_author_set_workspace_active`. That command was removed
// when routing moved to a conversation-tag model — the chat surface
// creates conversations with `surface_skill_id = "recipe-author"`
// and the runtime resolves the primary skill from that tag.
test.describe("recipe author workspace", () => {
  test("user opens workspace, creates a project, sees dashboard, exits", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // The nav-rail entry is gated behind enable_recipe_authoring,
    // which the shim default returns true — so it's present.
    const openBtn = page.getByTestId("nav-recipe-author");
    await expect(openBtn).toBeVisible();

    await openBtn.click();
    const workspace = page.getByTestId("recipe-author-workspace");
    await expect(workspace).toBeVisible();

    // Empty state shows the recipe-author welcome pane and "no project
    // selected" on the right.
    await expect(workspace).toContainText("Author a knowledge recipe");
    await expect(workspace).toContainText("No project selected.");

    // Open the new-project dialog.
    await page.getByTestId("recipe-author-new-project").click();
    const titleInput = page.getByTestId("recipe-author-new-title");
    const charterInput = page.getByTestId("recipe-author-new-charter");
    await expect(titleInput).toBeVisible();

    await titleInput.fill("Marcus — Ninth Circuit");
    await charterInput.fill(
      "# Charter\n\nFederal Ninth Circuit case law from 2020 onward.",
    );
    await page.getByTestId("recipe-author-new-submit").click();

    // The new project appears in the sidebar and is auto-selected;
    // the dashboard card grid renders.
    const row = page.getByTestId("recipe-author-project-row").first();
    await expect(row).toContainText("Marcus — Ninth Circuit");

    const dashboard = page.getByTestId("recipe-author-dashboard");
    await expect(dashboard).toBeVisible();
    await expect(dashboard).toContainText("Charter");
    await expect(dashboard).toContainText("Corpus state");
    await expect(dashboard).toContainText("Decisions");
    await expect(dashboard).toContainText("Capability requests");
    await expect(dashboard).toContainText("Checkpoints");
    await expect(dashboard).toContainText("Research log");
    await expect(dashboard).toContainText("Recipe TOML");

    // Chat surface shows the project header and the empty placeholder.
    const chatSurface = page.getByTestId("recipe-author-chat");
    await expect(chatSurface).toBeVisible();
    await expect(chatSurface).toContainText("Marcus — Ninth Circuit");
    await expect(chatSurface).toContainText("Describe the corpus you want to build");
    // Composer textarea is present and ready to receive input.
    await expect(page.getByTestId("recipe-author-composer")).toBeVisible();

    // Exit the workspace; back to chat.
    await page.getByText("← Back to chat").click();
    await expect(workspace).toHaveCount(0);
    await expect(page.locator(".empty-state, .chat-empty")).toBeVisible();
  });

  test("workspace surfaces the empty-state when no projects exist", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-recipe-author").click();
    const workspace = page.getByTestId("recipe-author-workspace");
    await expect(workspace).toBeVisible();
    // Sidebar shows the "no projects yet" text.
    await expect(workspace).toContainText("No recipe projects yet.");
  });

  test("dashboard renders populated cards when the backend has data", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Pre-seed a project + dashboard payload before opening the
    // workspace so the very first poll lands populated.
    await page.evaluate(() => {
      const featureId = "feat-test";
      window.__sovereign_test__.recipeAuthor.projects = [
        {
          feature_id: featureId,
          title: "Seeded project",
          charter_excerpt: "Charter excerpt…",
          recipe_id: "marcus-courtlistener",
          current_sample_size: 200,
          last_test_status: "pass",
          created_at: 1700000000,
          updated_at: 1700000100,
        },
      ];
      window.__sovereign_test__.recipeAuthor.dashboards[featureId] = {
        feature_id: featureId,
        title: "Seeded project",
        charter_md: "# Charter\n\nNinth Circuit federal case law from 2020.",
        recipe_id: "marcus-courtlistener",
        recipe_path: "/Users/test/.sovereign/recipes/marcus-courtlistener/recipe.toml",
        recipe_toml: '[corpus]\nid = "marcus-courtlistener"\n',
        current_sample_size: 200,
        last_test_status: "pass",
        last_test_at: "2026-05-04T12:00:00Z",
        created_at: 1700000000,
        updated_at: 1700000100,
        decisions: [
          {
            id: "n1",
            kind: "decision",
            content: "Chose CourtListener v4 API as primary source.",
            created_at: "2026-05-04T11:55:00Z",
            decision_kind: "source_choice",
            attribution: "partner",
            payload: {
              decision_kind: "source_choice",
              attribution: "partner",
            },
          },
        ],
        research_findings: [
          {
            id: "r1",
            kind: "research_finding",
            content: "v4 endpoint supports cluster__docket__court=ca9.",
            created_at: "2026-05-04T11:50:00Z",
            payload: {
              authority: "authoritative",
              source_url: "https://www.courtlistener.com/help/api/rest/",
            },
          },
        ],
        capability_requests: [],
        recipe_issues: [
          {
            id: "i1",
            kind: "recipe_issue",
            content: "1 of 50 docs had empty plain_text.",
            created_at: "2026-05-04T11:58:00Z",
            payload: { category: "extraction_zero", count: 1 },
          },
        ],
        deferred_questions: [],
        checkpoints: [
          {
            checkpoint_id: "1714826400-after-50-pass",
            name: "after 50-doc pass",
            trigger: "auto_scale_up",
            summary: "Sample passed at n=50; ready to climb to 200.",
            created_at: "2026-05-04T11:00:00Z",
          },
        ],
        validation: { ok: true, errors: [], no_recipe: false },
      };
    });

    await page.getByTestId("nav-recipe-author").click();

    // Pick the seeded project from the sidebar.
    const row = page.getByTestId("recipe-author-project-row").first();
    await expect(row).toContainText("Seeded project");
    await row.click();

    const dashboard = page.getByTestId("recipe-author-dashboard");
    await expect(dashboard).toBeVisible();

    // Card content from the seeded payload.
    await expect(dashboard).toContainText("source_choice");
    await expect(dashboard).toContainText("Chose CourtListener v4 API");
    await expect(dashboard).toContainText("authoritative");
    await expect(dashboard).toContainText("v4 endpoint supports cluster");
    await expect(dashboard).toContainText("extraction_zero");
    await expect(dashboard).toContainText("after 50-doc pass");
    await expect(dashboard).toContainText("marcus-courtlistener");

    // TOML drawer is collapsed by default; expand and assert content.
    await page.getByTestId("recipe-author-toml-toggle").click();
    await expect(dashboard).toContainText('id = "marcus-courtlistener"');
  });

  test("validation card surfaces parse errors front and center", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);

    // Seed a project whose on-disk recipe.toml fails to parse — the
    // backend's translate_parse_error has already rewritten the
    // message into partner-readable guidance; the glassbox card
    // must render that text verbatim and let the user copy it.
    await page.evaluate(() => {
      const featureId = "feat-broken";
      window.__sovereign_test__.recipeAuthor.projects = [
        {
          feature_id: featureId,
          title: "Broken recipe",
          charter_excerpt: "…",
          recipe_id: "broken",
          current_sample_size: null,
          last_test_status: "fail",
          created_at: 1700000000,
          updated_at: 1700000100,
        },
      ];
      window.__sovereign_test__.recipeAuthor.dashboards[featureId] = {
        feature_id: featureId,
        title: "Broken recipe",
        charter_md: "Charter…",
        recipe_id: "broken",
        recipe_path: "/Users/test/.sovereign/recipes/broken/recipe.toml",
        recipe_toml: "[corpus]\nid = \"broken\"\n",
        current_sample_size: null,
        last_test_status: "fail",
        last_test_at: "2026-05-04T12:00:00Z",
        created_at: 1700000000,
        updated_at: 1700000100,
        decisions: [],
        research_findings: [],
        capability_requests: [],
        recipe_issues: [],
        deferred_questions: [],
        checkpoints: [],
        validation: {
          ok: false,
          no_recipe: false,
          errors: [
            'Recipe is missing the [acquire] section. Every recipe needs one. Add it with type = "..." (one of: bulk_download | http_api | web_crawl | local_file | huggingface_dataset).',
            "field 'document_format' got 'pdf' but allowed values are: html, json, xml, plaintext",
          ],
        },
      };
    });

    await page.getByTestId("nav-recipe-author").click();
    await page.getByTestId("recipe-author-project-row").first().click();

    const dashboard = page.getByTestId("recipe-author-dashboard");
    await expect(dashboard).toBeVisible();

    // Card title + verdict + per-error blocks render.
    await expect(dashboard).toContainText("Recipe validation");
    await expect(dashboard).toContainText("needs attention");
    await expect(dashboard).toContainText("2 issues blocking the recipe");
    await expect(dashboard).toContainText("[acquire] section");
    await expect(dashboard).toContainText("allowed values are: html, json");

    // Copy button per error — at least one is present and clickable.
    const copyButtons = page.getByTestId("recipe-validation-copy");
    await expect(copyButtons.first()).toBeVisible();
  });

  test("workspace switcher is hidden when enable_recipe_authoring is false", async ({
    sovereignPage: page,
    chat,
  }) => {
    // Override the shim's default get_config BEFORE bootToChat runs
    // so the very first read by App.svelte returns the OFF state.
    await page.addInitScript(() => {
      // Wait for the shim's __sovereign_test__ to install before
      // overriding — the shim init is synchronous, but its handler
      // overrides only kick in after `setHandler` is callable.
      const tryInstall = () => {
        if (!window.__sovereign_test__) {
          setTimeout(tryInstall, 0);
          return;
        }
        window.__sovereign_test__.setHandler("get_config", () => ({
          embedding_model: null,
          chat_model: null,
          mesh_enabled: false,
          enable_recipe_authoring: false,
        }));
      };
      tryInstall();
    });
    await bootToChat(page, chat);

    // Sidebar entry must NOT appear when the flag is off.
    await expect(page.getByTestId("nav-recipe-author")).toHaveCount(0);
  });

  test("validation card shows the no-recipe state cleanly", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await page.getByTestId("nav-recipe-author").click();
    await page.getByTestId("recipe-author-new-project").click();
    await page.getByTestId("recipe-author-new-title").fill("Pristine");
    await page.getByTestId("recipe-author-new-charter").fill("# Charter");
    await page.getByTestId("recipe-author-new-submit").click();
    const dashboard = page.getByTestId("recipe-author-dashboard");
    await expect(dashboard).toContainText("Recipe validation");
    await expect(dashboard).toContainText("No recipe drafted yet.");
  });
});

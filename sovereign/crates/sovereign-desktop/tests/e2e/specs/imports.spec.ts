import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Settings → Imports — pin the load-bearing UX contract.
//
// Real-world regression that drove this spec: the user clicked
// "Import Claude export", saw "Starting…", then nothing changed for
// 20 minutes. Switching tabs reset the button as if the import was
// never started. Root causes were (a) the import state was held in
// component-local `$state` and lost on unmount, and (b) the v2 atlas
// enrichment subprocess never ran post-ingest, so the progress card
// had nothing to report once the ingest phases completed.
//
// These tests pin three properties:
//
//   1. State survives navigating away from Settings → Imports and
//      back — the progress card and stage label persist.
//   2. After ingest reports `phase: "complete"`, the store fires
//      `enrich_build_async` and the phase label flips to the
//      enrichment step ("Reading every conversation", etc.). The
//      progress bar stays > 0% across the handoff.
//   3. After the enrichment subprocess emits `kind: "complete"`,
//      the "Open in Atlas" button appears and clicking it sets
//      `atlasNavigation.pendingAtom` so App.svelte switches the
//      rail.

const CORPUS_ID = "conversations-anthropic";

async function openSettings(page: Page, chat: Parameters<typeof bootToChat>[1]) {
  await bootToChat(page, chat);
  await page.getByTestId("nav-settings").click();
  await page.locator(".cfg").waitFor();
}

async function clickImportsTab(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Imports" }).click();
  await page.getByText("Claude (Anthropic)").waitFor();
}

async function stubBaselineHandlers(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as {
      __sovereign_test__: {
        setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
      };
    };
    // Dialog file picker — return the canonical Anthropic export path.
    w.__sovereign_test__.setHandler("plugin:dialog|open", () => {
      return "/tmp/test-anthropic-export.zip";
    });
    // Tauri command: the import_anthropic_zip backend. Returns a
    // realistic pre-flight estimate so the band renders.
    w.__sovereign_test__.setHandler("import_anthropic_zip", () => ({
      kind: "started",
      corpus_id: "conversations-anthropic",
      total_messages: 12_000,
      estimated_minutes: 80,
      canonical_path: "/home/test/.sovereign/conversations/conversations.json",
    }));
    // Enrichment subprocess — return a fake job handle. The test
    // pumps `EnrichProgress` events on the returned channel.
    w.__sovereign_test__.setHandler("enrich_build_async", () => ({
      job_id: "test-job-001",
      corpus_id: "conversations-anthropic",
      channel: "enrich://progress/test-job-001",
    }));
    // Atom list — one fake atom so "Open in Atlas" finds something
    // to focus on.
    w.__sovereign_test__.setHandler("atlas_list_atoms", () => ({
      items: [
        {
          atom_id: "entity-0001",
          stable_key: "test-key-001",
          atom_type: "Entity",
          display_name: "Test entity",
          enrichment_depth: "extracted",
          evidence_chunk_count: 1,
          curation_status: "generated",
          overlay_supports: false,
        },
      ],
      total_matching: 1,
    }));
  });
}

test.describe("Settings → Imports", () => {
  test("state survives tab navigation; the progress card persists", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await stubBaselineHandlers(page);
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();

    // Pre-flight estimate landed → progress card is up.
    await expect(page.getByTestId("imports-progress-card")).toBeVisible();
    await expect(
      page.getByText(/12,000 messages/),
    ).toBeVisible();

    // Drive one ingest-side progress tick so the stage flips to
    // "ingesting" with a visible phase label.
    await page.evaluate((corpusId) => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("corpus-progress", {
        corpus_id: corpusId,
        phase: "extracting",
        percent: 15,
        chunks_processed: 1800,
      });
    }, CORPUS_ID);

    await expect(page.getByTestId("imports-phase-label")).toContainText(
      /Extracting conversations/,
    );

    // Navigate to a different tab and back. v1 of this spec failed
    // here: the component remounted to an "Idle" panel and the
    // progress card vanished. The fix lifted state into a module-
    // level Svelte store; this assertion is the regression gate.
    await page.getByRole("button", { name: "Models" }).click();
    await page.getByRole("button", { name: "Imports" }).click();

    await expect(page.getByTestId("imports-progress-card")).toBeVisible();
    await expect(page.getByText(/12,000 messages/)).toBeVisible();
    await expect(page.getByTestId("imports-phase-label")).toContainText(
      /Extracting conversations/,
    );
  });

  test("ingest complete auto-triggers enrichment; complete reveals Open in Atlas", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await stubBaselineHandlers(page);
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();
    await expect(page.getByTestId("imports-progress-card")).toBeVisible();

    // Pump ingest → complete in one go to exercise the auto-handoff.
    await page.evaluate((corpusId) => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("corpus-progress", {
        corpus_id: corpusId,
        phase: "complete",
        percent: 100,
        chunks_processed: 12000,
      });
    }, CORPUS_ID);

    // The store should have called `enrich_build_async` (stubbed
    // above) and started listening on the returned channel. Drive
    // a step_start to verify the new stage label rendered.
    await page.waitForTimeout(50);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("enrich://progress/test-job-001", {
        kind: "step_start",
        corpus_id: "conversations-anthropic",
        step: "extract",
        ordinal: 2,
        total: 7,
      });
    });

    await expect(page.getByTestId("imports-phase-label")).toContainText(
      /Reading every conversation/,
    );

    // Drive the enrichment complete event.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("enrich://progress/test-job-001", {
        kind: "complete",
        corpus_id: "conversations-anthropic",
        steps_completed: 7,
      });
    });

    await expect(page.getByTestId("imports-open-in-atlas")).toBeVisible();

    // Clicking "Open in Atlas" should switch the rail to the Atlas
    // view. App.svelte observes the `atlasNavigation` store's
    // `pendingAtom`, flips `view` to `"atlas"`, and mounts
    // `AtlasSurface`. The `.atlas-surface` div is the load-bearing
    // visible marker once the switch lands.
    await page.getByTestId("imports-open-in-atlas").click();
    await expect(page.locator(".atlas-surface")).toBeVisible({
      timeout: 10_000,
    });
  });

  test("file picker cancellation leaves the panel idle", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    // Cancellation: dialog returns null.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("plugin:dialog|open", () => null);
    });
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();
    // Progress card never appears when no file was picked.
    await expect(page.getByTestId("imports-progress-card")).toHaveCount(0);
    await expect(page.getByTestId("imports-pick-claude")).toBeEnabled();
  });

  test("partial-index response surfaces destructive-confirm banner; click re-invokes with reset_partial", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    // First call: report a partial index. Second call (after user
    // confirms): start cleanly. We track invocation order from
    // inside the page.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      // Stub the dialog + cheap-look handlers.
      w.__sovereign_test__.setHandler(
        "plugin:dialog|open",
        () => "/tmp/test-anthropic-export.zip",
      );
      w.__sovereign_test__.setHandler("atlas_list_atoms", () => ({
        items: [],
        total_matching: 0,
      }));
      w.__sovereign_test__.setHandler("enrich_build_async", () => ({
        job_id: "job-x",
        corpus_id: "conversations-anthropic",
        channel: "enrich://progress/job-x",
      }));
      // Capture invocations on a window-attached probe so the test
      // can assert the second call carried `reset_partial: true`.
      (
        window as unknown as { __import_calls__: Array<unknown> }
      ).__import_calls__ = [];
      w.__sovereign_test__.setHandler("import_anthropic_zip", (args) => {
        const probe = (
          window as unknown as { __import_calls__: Array<unknown> }
        ).__import_calls__;
        probe.push(args);
        if (probe.length === 1) {
          return {
            kind: "partial_index_exists",
            corpus_id: "conversations-anthropic",
            index_path: "/home/test/.sovereign/indexes/conversations-anthropic",
            total_messages: 8_500,
            estimated_minutes: 60,
            canonical_path:
              "/home/test/.sovereign/conversations/conversations.json",
          };
        }
        return {
          kind: "started",
          corpus_id: "conversations-anthropic",
          total_messages: 8_500,
          estimated_minutes: 60,
          canonical_path:
            "/home/test/.sovereign/conversations/conversations.json",
        };
      });
    });
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();
    // Confirmation banner appears; progress card does NOT.
    await expect(page.getByTestId("imports-reset-confirm")).toBeVisible();
    await expect(page.getByTestId("imports-progress-card")).toHaveCount(0);

    await page.getByTestId("imports-reset-confirm-yes").click();

    // Second invocation should have happened, with reset_partial: true.
    await expect(page.getByTestId("imports-progress-card")).toBeVisible();
    const callsCount = await page.evaluate(() =>
      (window as unknown as { __import_calls__: Array<unknown> })
        .__import_calls__.length,
    );
    expect(callsCount).toBe(2);
    const secondCall = await page.evaluate(() => {
      const calls = (
        window as unknown as { __import_calls__: Array<Record<string, unknown>> }
      ).__import_calls__;
      return calls[1] as { request?: Record<string, unknown> };
    });
    expect(secondCall.request).toEqual({
      zip_path: "/tmp/test-anthropic-export.zip",
      reset_partial: true,
    });
  });

  test("partial-index response: Cancel returns to idle without invoking again", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler(
        "plugin:dialog|open",
        () => "/tmp/test-anthropic-export.zip",
      );
      (
        window as unknown as { __import_calls__: Array<unknown> }
      ).__import_calls__ = [];
      w.__sovereign_test__.setHandler("import_anthropic_zip", (args) => {
        (
          window as unknown as { __import_calls__: Array<unknown> }
        ).__import_calls__.push(args);
        return {
          kind: "partial_index_exists",
          corpus_id: "conversations-anthropic",
          index_path: "/home/test/.sovereign/indexes/conversations-anthropic",
          total_messages: 8_500,
          estimated_minutes: 60,
          canonical_path:
            "/home/test/.sovereign/conversations/conversations.json",
        };
      });
    });
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();
    await expect(page.getByTestId("imports-reset-confirm")).toBeVisible();
    await page.getByTestId("imports-reset-confirm-cancel").click();
    await expect(page.getByTestId("imports-reset-confirm")).toHaveCount(0);
    await expect(page.getByTestId("imports-pick-claude")).toBeEnabled();
    // Only the original probe call — no destructive re-invoke.
    const callsCount = await page.evaluate(() =>
      (window as unknown as { __import_calls__: Array<unknown> })
        .__import_calls__.length,
    );
    expect(callsCount).toBe(1);
  });

  test("import_anthropic_zip rejection surfaces an inline error", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openSettings(page, chat);
    await stubBaselineHandlers(page);
    // Override: the Tauri command throws (e.g. no conversations.json inside).
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("import_anthropic_zip", () => {
        throw new Error("no conversations.json in archive");
      });
    });
    await clickImportsTab(page);

    await page.getByTestId("imports-pick-claude").click();
    await expect(page.getByTestId("imports-error")).toContainText(
      /conversations\.json/,
    );
    await expect(page.getByTestId("imports-retry")).toBeVisible();
  });
});

// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat } from "../fixtures/test-base";
import type { Page } from "@playwright/test";

// Settings → Imports — pin the load-bearing UX contract.
//
// Real-world regression that drove this spec: the user clicked
// "Import Claude export", saw "Starting…", then nothing changed for
// 20 minutes. Switching tabs reset the button as if the import was
// never started. Root causes were (a) the import state was held in
// component-local `$state` and lost on unmount, and (b) post-ingest
// enrichment never ran, so the progress card had nothing to report
// once the ingest phases completed.
//
// Enrichment now runs IN-PROCESS in the daemon (there is no
// `sovereign-cli enrich build` subprocess — the CLI isn't bundled),
// so the store observes it by polling `lc_enrichment_status` rather
// than listening on a job channel. These tests pin three properties:
//
//   1. State survives navigating away from Settings → Imports and
//      back — the progress card and stage label persist.
//   2. After ingest reports `phase: "complete"`, the store enters the
//      "enriching" stage and polls `lc_enrichment_status`; the phase
//      label reflects the polled enrichment phase.
//   3. When the polled status reaches `phase: "complete"`, the "Open
//      in Atlas" button appears and clicking it sets
//      `atlasNavigation.pendingAtom` so App.svelte switches the rail.

const CORPUS_ID = "conversations-anthropic";

async function openLibraryAdd(page: Page, chat: Parameters<typeof bootToChat>[1]) {
  // Conversation imports moved from Settings → Imports to Library → Add →
  // Conversations (ImportsTab re-parented into AddSheet, Phase 1 refactor).
  await bootToChat(page, chat);
  await page.getByTestId("nav-library").click();
  await page.getByTestId("library-add").click();
}

async function openConversations(page: Page): Promise<void> {
  await page.getByTestId("add-section-imports").click();
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

test.describe("Library → Add → Conversations (imports)", () => {
  test("state survives tab navigation; the progress card persists", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    await stubBaselineHandlers(page);
    await openConversations(page);

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

    // Switch the Add sheet to a different section and back. v1 of this
    // spec failed here: the component remounted to an "Idle" panel and
    // the progress card vanished. The fix lifted state into a module-
    // level Svelte store; this assertion is the regression gate.
    await page.getByTestId("add-section-files").click();
    await page.getByTestId("add-section-imports").click();

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
    await openLibraryAdd(page, chat);
    await stubBaselineHandlers(page);

    // Controllable in-process enrichment status: the store polls
    // `lc_enrichment_status` after ingest completes. A window-attached
    // phase var lets this test step it building → complete instead of
    // pumping a (now-removed) subprocess channel.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
        __enrichPhase?: string;
      };
      w.__enrichPhase = "entity_extraction";
      w.__sovereign_test__.setHandler("lc_enrichment_status", () => {
        const phase = w.__enrichPhase ?? null;
        return {
          corpus_id: "conversations-anthropic",
          state: phase
            ? { phase, message: null, step_current: 0, step_total: 0 }
            : null,
          is_terminal: phase === "complete",
          is_stalled: false,
          fraction_complete: phase === "complete" ? 1 : 0.4,
        };
      });
    });

    await openConversations(page);

    await page.getByTestId("imports-pick-claude").click();
    await expect(page.getByTestId("imports-progress-card")).toBeVisible();

    // Pump ingest → complete. autoEnrich (Claude) → the store enters
    // the "enriching" stage and starts polling `lc_enrichment_status`.
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

    // First poll returns the `entity_extraction` phase → the card
    // renders its in-process phase caption.
    await expect(page.getByTestId("imports-phase-label")).toContainText(
      /Finding people, places, and ideas/,
    );

    // Flip the polled status to complete; the next poll tick (≤2s)
    // flips the store to the terminal `complete` stage.
    await page.evaluate(() => {
      (window as unknown as { __enrichPhase?: string }).__enrichPhase =
        "complete";
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
    await openLibraryAdd(page, chat);
    // Cancellation: dialog returns null.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("plugin:dialog|open", () => null);
    });
    await openConversations(page);

    await page.getByTestId("imports-pick-claude").click();
    // Progress card never appears when no file was picked.
    await expect(page.getByTestId("imports-progress-card")).toHaveCount(0);
    await expect(page.getByTestId("imports-pick-claude")).toBeEnabled();
  });

  test("partial-index response surfaces destructive-confirm banner; click re-invokes with reset_partial", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
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
    await openConversations(page);

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
    await openLibraryAdd(page, chat);
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
    await openConversations(page);

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

  test("auto-resumes from daemon state on app start; hides picker", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    // Stub the resume probe + seed localStorage BEFORE clicking
    // the Imports tab. The store calls `get_corpus_progress` on
    // init; a non-terminal payload should flip the stage straight
    // to "ingesting" and suppress the picker. localStorage carries
    // the prior pre-flight so the progress card renders the
    // message count + estimate band even across a desktop restart.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler(
        "get_corpus_progress",
        (args: unknown) => {
          const a = args as { corpusId?: string };
          if (a.corpusId !== "conversations-anthropic") return null;
          return {
            corpus_id: "conversations-anthropic",
            phase: "embedding",
            percent: 42,
            chunks_processed: 4200,
          };
        },
      );
      localStorage.setItem(
        "imports.lastStartResponse.v1",
        JSON.stringify({
          kind: "started",
          corpus_id: "conversations-anthropic",
          total_messages: 10_500,
          estimated_minutes: 70,
          canonical_path:
            "/home/test/.sovereign/conversations/conversations.json",
        }),
      );
    });
    // Don't use `openConversations` here — its `Claude (Anthropic)`
    // text-wait depends on the picker, and the picker is the exact
    // thing this test asserts is suppressed. Open the section + wait on
    // the resume banner instead.
    await page.getByTestId("add-section-imports").click();
    await page.getByTestId("imports-resume-banner").waitFor();

    // Claude's picker is suppressed; its resume banner + progress card
    // are up. Multi-source: ChatGPT's picker stays available alongside
    // (the shared `imports-sources` wrapper no longer vanishes).
    await expect(page.getByTestId("imports-pick-claude")).toHaveCount(0);
    await expect(page.getByTestId("imports-pick-chatgpt")).toBeVisible();
    await expect(page.getByTestId("imports-resume-banner")).toBeVisible();
    await expect(page.getByTestId("imports-progress-card")).toBeVisible();
    await expect(page.getByText(/10,500 messages/)).toBeVisible();
    await expect(page.getByTestId("imports-phase-label")).toContainText(
      /Embedding chunks/,
    );
  });

  test("auto-resume hidden when daemon reports no in-flight import", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("get_corpus_progress", () => null);
      localStorage.removeItem("imports.lastStartResponse.v1");
    });
    await openConversations(page);

    // No in-flight import → picker visible, banner + progress card absent.
    await expect(page.getByTestId("imports-sources")).toBeVisible();
    await expect(page.getByTestId("imports-resume-banner")).toHaveCount(0);
    await expect(page.getByTestId("imports-progress-card")).toHaveCount(0);
  });

  test("terminal phase from daemon does NOT trigger auto-resume", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      // Stale terminal entry — should be ignored.
      w.__sovereign_test__.setHandler("get_corpus_progress", () => ({
        corpus_id: "conversations-anthropic",
        phase: "complete",
        percent: 100,
        chunks_processed: 8000,
      }));
      localStorage.removeItem("imports.lastStartResponse.v1");
    });
    await openConversations(page);

    await expect(page.getByTestId("imports-sources")).toBeVisible();
    await expect(page.getByTestId("imports-resume-banner")).toHaveCount(0);
  });

  test("import_anthropic_zip rejection surfaces an inline error", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
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
    await openConversations(page);

    await page.getByTestId("imports-pick-claude").click();
    await expect(page.getByTestId("imports-error")).toContainText(
      /conversations\.json/,
    );
    await expect(page.getByTestId("imports-retry")).toBeVisible();
  });

  // ─── ChatGPT (OpenAI) source ────────────────────────────────────
  //
  // The ChatGPT card shares ConversationImportCard + the import state
  // machine with Claude; only the corpus id, import command, and
  // test-id prefix (`imports-chatgpt`) differ. This pins that the
  // second source drives its own progress AND stays independent of the
  // Claude card — the load-bearing property of the multi-source
  // refactor (each store filters the shared `corpus-progress` channel
  // to its own corpus id).
  test("ChatGPT import drives its own progress, independent of the Claude card", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
      };
      w.__sovereign_test__.setHandler("plugin:dialog|open", () => {
        return "/tmp/test-chatgpt-export.zip";
      });
      w.__sovereign_test__.setHandler("import_chatgpt_zip", () => ({
        kind: "started",
        corpus_id: "conversations-chatgpt",
        total_messages: 8_000,
        estimated_minutes: 53,
        canonical_path:
          "/home/test/.sovereign/conversations-chatgpt/conversations.json",
      }));
    });
    await openConversations(page);

    await page.getByTestId("imports-pick-chatgpt").click();

    // ChatGPT's own progress card is up with its message count.
    await expect(page.getByTestId("imports-chatgpt-progress-card")).toBeVisible();
    await expect(page.getByText(/8,000 messages/)).toBeVisible();

    // Independence: the Claude card is untouched — its picker is still
    // offered and no Claude progress card exists.
    await expect(page.getByTestId("imports-pick-claude")).toBeVisible();
    await expect(page.getByTestId("imports-progress-card")).toHaveCount(0);

    // A `corpus-progress` tick for the ChatGPT corpus advances only the
    // ChatGPT card.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("corpus-progress", {
        corpus_id: "conversations-chatgpt",
        phase: "extracting",
        percent: 20,
        chunks_processed: 1600,
      });
    });
    await expect(page.getByTestId("imports-chatgpt-phase-label")).toContainText(
      /Extracting conversations/,
    );

    // Cross-talk guard: a tick for the *Claude* corpus must NOT leak
    // into the ChatGPT card (it filters by corpus id).
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("corpus-progress", {
        corpus_id: "conversations-anthropic",
        phase: "embedding",
        percent: 90,
        chunks_processed: 9000,
      });
    });
    await expect(page.getByTestId("imports-chatgpt-phase-label")).toContainText(
      /Extracting conversations/,
    );
  });

  // Email import — the third card. Pins the two properties that differ
  // from the chat imports: (a) the picked path travels to the backend
  // command as-is (it becomes the recipe's `path` parameter — there is
  // no staging copy), and (b) NO auto-enrichment: the email-archive
  // recipe ships `[enrichment]` off, so ingest `complete` is terminal —
  // the completion note replaces "Open in Atlas" and the store never
  // enters the enriching stage (so it never polls `lc_enrichment_status`
  // for this corpus).
  test("email import completes at ingest with no enrichment hop", async ({
    sovereignPage: page,
    chat,
  }) => {
    await openLibraryAdd(page, chat);
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: {
          setHandler: (cmd: string, fn: (args: unknown) => unknown) => void;
        };
        __emailImportArgs?: unknown;
        __enrichPolls?: number;
      };
      w.__enrichPolls = 0;
      w.__sovereign_test__.setHandler("plugin:dialog|open", () => {
        return "/tmp/takeout.mbox";
      });
      w.__sovereign_test__.setHandler("import_email_archive", (args) => {
        w.__emailImportArgs = args;
        return {
          kind: "started",
          corpus_id: "email-archive",
          total_messages: 6_214,
          estimated_minutes: 5.2,
          canonical_path: "/tmp/takeout.mbox",
        };
      });
      // Count enrichment polls for the email corpus — must stay 0
      // because `autoEnrich: false` treats ingest completion as
      // terminal (no enriching stage, no poll).
      w.__sovereign_test__.setHandler("lc_enrichment_status", (args) => {
        const a = args as { corpusId?: string };
        if (a.corpusId === "email-archive") {
          w.__enrichPolls = (w.__enrichPolls ?? 0) + 1;
        }
        return {
          corpus_id: a.corpusId ?? "email-archive",
          state: null,
          is_terminal: false,
          is_stalled: false,
          fraction_complete: 0,
        };
      });
    });
    await openConversations(page);

    // Both pick modes are offered (file for .mbox/.eml, folder for
    // maildir), and the privacy-forward card is present.
    await expect(page.getByText("Email (your own mailbox)")).toBeVisible();
    await expect(page.getByTestId("imports-email-pick-folder")).toBeVisible();

    await page.getByTestId("imports-pick-email").click();
    await expect(page.getByTestId("imports-email-progress-card")).toBeVisible();
    await expect(page.getByText(/6,214 messages/)).toBeVisible();

    // The backend command received the picked path verbatim.
    const args = await page.evaluate(
      () => (window as unknown as { __emailImportArgs?: unknown }).__emailImportArgs,
    );
    expect(JSON.stringify(args)).toContain("/tmp/takeout.mbox");

    // Ingest completes → terminal. Note shown, no atlas button, and the
    // enrichment subprocess was never spawned for this corpus.
    await page.evaluate(() => {
      const w = window as unknown as {
        __sovereign_test__: { emit: (eventName: string, payload: unknown) => number };
      };
      w.__sovereign_test__.emit("corpus-progress", {
        corpus_id: "email-archive",
        phase: "complete",
        percent: 100,
        chunks_processed: 6214,
      });
    });
    await expect(page.getByTestId("imports-email-complete-note")).toBeVisible();
    await expect(page.getByTestId("imports-email-open-in-atlas")).toHaveCount(0);
    // Give any stray poll a tick to fire, then assert none did.
    await page.waitForTimeout(100);
    const enrichPolls = await page.evaluate(
      () => (window as unknown as { __enrichPolls?: number }).__enrichPolls,
    );
    expect(enrichPolls).toBe(0);
  });
});

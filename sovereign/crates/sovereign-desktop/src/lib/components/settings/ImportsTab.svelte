<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Settings → Imports
  //
  // Composes one <ConversationImportCard> per chat source (Claude,
  // ChatGPT), each bound to its own module-singleton import store, plus
  // the shared GliNER "smart highlights" model card and a Gemini
  // "Coming soon" placeholder. The cards survive unmount (state lives in
  // the stores), listen to `corpus-progress` globally, and auto-fire
  // `enrich_build_async` when ingest completes — so navigating away from
  // this tab and back never resets an in-flight import.
  //
  // Adding a vendor = one more <ConversationImportCard> + its store
  // instance + extractor/recipe (SYSTEM_OVERVIEW §10.1). Gemini stays a
  // disabled placeholder until its extractor lands.

  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    atlasCheckGlinerModel,
    atlasDownloadGlinerModel,
    importAnthropicZip,
    importChatgptZip,
    importEmailArchive,
  } from "../../api";
  import type { GlinerModelStatus } from "../../types";
  import {
    anthropicImportsStore,
    chatgptImportsStore,
    emailImportsStore,
  } from "../../stores/importsStore.svelte";
  import ConversationImportCard from "./ConversationImportCard.svelte";

  // ─── GliNER per-chunk entity extraction (Phase 1 model UX) ───
  //
  // Local NER model for the conv-tiered retrieval surface. Without
  // it, ingest still works but falls back to RAPTOR-derived
  // entities only (~5/leaf instead of ~24/chunk). The toggle is a
  // one-time install — once the model is on disk, every future
  // import auto-runs entity extraction via the daemon hook. Shared
  // across all sources (it's about the model, not the vendor).
  let glinerStatus: GlinerModelStatus | null = $state(null);
  let glinerDownloading = $state(false);
  let glinerDownloadFile: string | null = $state(null);
  let glinerDownloadPct = $state(0);
  let glinerError: string | null = $state(null);

  onMount(() => {
    void refreshGlinerStatus();
    const unlisten = listen<{ file: string; downloaded: number; total: number }>(
      "gliner-download-progress",
      (e) => {
        const { file, downloaded, total } = e.payload;
        if (file === "__complete__") {
          glinerDownloadFile = null;
          glinerDownloadPct = 0;
          void refreshGlinerStatus();
          return;
        }
        glinerDownloadFile = file;
        if (total > 0) {
          glinerDownloadPct = Math.min(100, Math.round((downloaded / total) * 100));
        }
      },
    );
    return () => {
      void unlisten.then((u) => u());
    };
  });

  async function refreshGlinerStatus() {
    try {
      // Coerce a missing result to `null` so the `=== null` template
      // guard catches it — otherwise `undefined` slips past and
      // `glinerStatus.installed` throws, blanking the Imports panel.
      glinerStatus = (await atlasCheckGlinerModel()) ?? null;
    } catch (e) {
      glinerError = e instanceof Error ? e.message : String(e);
    }
  }

  async function downloadGlinerModel() {
    glinerDownloading = true;
    glinerError = null;
    glinerDownloadPct = 0;
    glinerDownloadFile = null;
    try {
      await atlasDownloadGlinerModel();
    } catch (e) {
      glinerError = e instanceof Error ? e.message : String(e);
    } finally {
      glinerDownloading = false;
      await refreshGlinerStatus();
    }
  }
</script>

<div class="imports-tab">
  <div class="sources" data-testid="imports-sources">
    <ConversationImportCard
      store={anthropicImportsStore}
      importFn={importAnthropicZip}
      sourceName="Claude (Anthropic)"
      progressLabel="Claude conversations"
      importLabel="Import Claude export"
      fileFilterName="Claude export (.zip)"
      pickTestId="imports-pick-claude"
      testidPrefix="imports"
      help={claudeHelp}
    />

    <ConversationImportCard
      store={chatgptImportsStore}
      importFn={importChatgptZip}
      sourceName="ChatGPT (OpenAI)"
      progressLabel="ChatGPT conversations"
      importLabel="Import ChatGPT export"
      fileFilterName="ChatGPT export (.zip)"
      pickTestId="imports-pick-chatgpt"
      testidPrefix="imports-chatgpt"
      help={chatgptHelp}
    />

    <ConversationImportCard
      store={emailImportsStore}
      importFn={importEmailArchive}
      sourceName="Email (your own mailbox)"
      progressLabel="Email archive"
      importLabel="Import mailbox export"
      fileFilterName="Email archive (.mbox / .eml)"
      pickExtensions={["mbox", "eml"]}
      folderPickLabel="Import a mail folder instead (maildir / .eml)"
      folderPickTestId="imports-email-pick-folder"
      pickTestId="imports-pick-email"
      testidPrefix="imports-email"
      icon="✉️"
      completeNote="Done — your mailbox is now a notebook in the Library. Open it and Ask; every answer cites the original message. It stays on this machine: never shared to the mesh, never replicated, never queried by peers."
      help={emailHelp}
    />

    <article class="source-card source-card--disabled">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">Gemini (Google)</h3>
          <p class="source-help">Export Gemini Apps via Google Takeout. Support coming soon.</p>
        </div>
      </header>
      <span class="badge">Coming soon</span>
    </article>
  </div>

  <!-- Smart highlights (GliNER per-chunk NER). One-time model
       install; thereafter every imported conversation gets
       automatic entity tagging used by search + Atlas. -->
  <article class="gliner-card">
    <header class="source-header">
      <div class="source-icon">🔍</div>
      <div class="source-meta">
        <h3 class="source-name">Smart highlights for your chats</h3>
        <p class="source-help">
          Tags the people, places, works, and organizations across every
          conversation you import. Runs in the background once installed —
          and search starts finding related threads across topics you
          didn't think to link.
        </p>
      </div>
    </header>
    <div class="gliner-controls" data-testid="gliner-controls">
      {#if glinerStatus === null}
        <span class="badge">Checking…</span>
      {:else if glinerStatus.installed}
        <span class="badge installed">✓ On</span>
        <span class="path-hint">runs locally · nothing leaves your machine</span>
        <button
          type="button"
          class="redownload-btn"
          onclick={downloadGlinerModel}
          disabled={glinerDownloading}
          title="Re-download the model files (skip if already present)"
        >
          {glinerDownloading ? "Updating…" : "Re-download"}
        </button>
      {:else if glinerDownloading}
        <span class="badge running">
          {#if glinerDownloadFile === "tokenizer.json"}
            Preparing… {glinerDownloadPct}%
          {:else}
            Downloading… {glinerDownloadPct}%
          {/if}
        </span>
      {:else}
        <span class="badge not-installed">Off</span>
        <button
          type="button"
          class="install-btn"
          onclick={downloadGlinerModel}
        >
          Turn on (one-time {glinerStatus.size_estimate_mb} MB download)
        </button>
      {/if}
    </div>
    {#if glinerError}
      <p class="gliner-error" role="alert">
        Something went wrong: {glinerError}
      </p>
    {/if}
  </article>
</div>

{#snippet claudeHelp()}
  Go to <strong>claude.ai → Settings → Privacy → Export data</strong>.
  Anthropic emails a download link. It's a <code>.zip</code> named
  <code>data-&lt;uuid&gt;-&lt;batch&gt;.zip</code>.
{/snippet}

{#snippet chatgptHelp()}
  Go to <strong>ChatGPT → Settings → Data controls → Export data</strong>.
  OpenAI emails a download link with a <code>.zip</code> of your
  conversations.
{/snippet}

{#snippet emailHelp()}
  <strong>Gmail:</strong> Google Takeout → Mail → download the
  <code>.mbox</code>. <strong>Apple Mail:</strong> select a mailbox →
  File → Export Mailbox…. Thunderbird stores, maildir folders, and
  <code>.eml</code> files work too — formats are detected by content.
  Read in place; nothing is uploaded anywhere.
{/snippet}

<style>
  .imports-tab {
    display: flex;
    flex-direction: column;
    gap: 24px;
    max-width: 720px;
  }

  .sources {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .source-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 18px 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .source-card--disabled {
    opacity: 0.6;
  }

  .source-card-header {
    display: flex;
    align-items: flex-start;
    gap: 14px;
    flex: 1;
  }

  .source-icon {
    font-size: 1.6rem;
    line-height: 1;
    margin-top: 2px;
  }

  .source-meta {
    flex: 1;
  }

  .source-name {
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 4px;
    letter-spacing: -0.01em;
  }

  .source-help {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.85rem;
    line-height: 1.5;
  }

  .badge {
    padding: 3px 9px;
    background: var(--bg-elevated, var(--bg-primary));
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.74rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  /* GliNER per-chunk entity extraction (Phase 1 install card) */
  .gliner-card {
    margin-top: 16px;
    padding: 16px;
    background: var(--bg-surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .gliner-controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .gliner-controls .badge.installed {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .gliner-controls .badge.not-installed {
    background: var(--bg-elevated);
    color: var(--text-muted);
  }
  .gliner-controls .badge.running {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    font-variant-numeric: tabular-nums;
  }
  .install-btn,
  .redownload-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: 6px;
    padding: 6px 14px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .install-btn:hover:not(:disabled),
  .redownload-btn:hover:not(:disabled) {
    background: var(--accent-strong, #6989f0);
  }
  .install-btn:disabled,
  .redownload-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .path-hint {
    font-size: 0.72rem;
    color: var(--text-muted, #888);
    font-family: var(--font-mono);
  }
  .gliner-error {
    margin: 0;
    color: var(--error, #d44);
    font-size: 0.85rem;
  }
</style>

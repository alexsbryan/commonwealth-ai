<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Glassbox card: turn a drafted recipe into a built, ENRICHED corpus —
  // without leaving the Recipe Author. Reuses the existing paths the corpus
  // library uses, in sequence:
  //   1. `installCorpus` — ingest (acquire → extract → chunk → embed → index),
  //      progress on the shared `corpusProgressStore`.
  //   2. when the recipe will produce atoms (`enrichmentReady`):
  //      a. `recipeEnrichInitFromCorpus` — the BRIDGE: scaffold the atlas config
  //         straight from the just-installed index (ingest of a `type="atlas"`
  //         text recipe runs the field-model enricher, which writes no atoms, so
  //         `enrich build` needs this config first).
  //      b. `enrichBuildAsync` — the atlas pipeline that writes `atlas/atoms.json`,
  //         progress on `enrich://progress/{job}`.
  // After this, "Verify enrichment" (HarnessLadderCard) confirms the atoms.
  // No new ingest/enrich engine — orchestration over commands that already exist.
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import Card from "./Card.svelte";
  import StarterChips from "../StarterChips.svelte";
  import {
    installCorpus,
    enrichBuildAsync,
    recipeEnrichInitFromCorpus,
    enrichGetStarterQuestions,
  } from "../../api";
  import { corpusProgressStore } from "../../stores/corpusProgress.svelte";
  import { phaseLabel } from "../knowledgeStatusFormat";
  import type { EnrichProgress, StarterQuestion } from "../../types";

  // `onUseInChat` (seed a starter question + navigate) and `onOpenChat`
  // (navigate, no seed) are the "land in use" handoff — the host
  // (App.svelte) provides the same two handlers Settings → Knowledge
  // uses (`handleSettingsStarterPick` / `handleDropToChat`). Optional so
  // the card still renders standalone (tests, storybook) without them.
  let {
    recipeId,
    enrichmentReady,
    onUseInChat,
    onOpenChat,
  }: {
    recipeId: string | null;
    enrichmentReady: boolean;
    onUseInChat?: (question: StarterQuestion) => void;
    onOpenChat?: () => void;
  } = $props();

  type Stage = "idle" | "building" | "enriching" | "done" | "failed";
  let stage = $state<Stage>("idle");
  let detail = $state<string>("");
  let error = $state<string | null>(null);

  // Mined "use this corpus" chips, populated once the build reaches
  // `done`. Empty for a built-but-not-enriched corpus (no atlas to mine)
  // — the plain "Open in chat" action still appears. `recipeId` IS the
  // corpus id here (install/init/build all key off it).
  let starters = $state<StarterQuestion[]>([]);
  async function loadStarters(): Promise<void> {
    if (!recipeId) return;
    try {
      starters = await enrichGetStarterQuestions(recipeId, 3);
    } catch {
      // A missing/half-built atlas is not an error here — just no chips.
      starters = [];
    }
  }

  // One-shot guard + listener handle (plain refs — not display state).
  let enrichStarted = false;
  let enrichUnlisten: (() => void) | null = null;

  onMount(() => {
    void corpusProgressStore.init();
    return () => enrichUnlisten?.();
  });

  // Live ingest progress for this corpus (shared reactive store).
  let install = $derived(recipeId ? corpusProgressStore.byId[recipeId] : undefined);

  // Chain: when ingest completes, kick off enrichment once (if the recipe will
  // produce atoms); otherwise the build is done.
  $effect(() => {
    if (stage !== "building" || !recipeId) return;
    const p = install;
    if (!p) return;
    if (p.phase === "failed") {
      stage = "failed";
      error = p.message || "build failed during ingest";
    } else if (p.phase === "complete" && !enrichStarted) {
      enrichStarted = true;
      if (enrichmentReady) {
        void startEnrich(recipeId);
      } else {
        stage = "done";
        detail = "built (no enrichment configured)";
        void loadStarters();
      }
    }
  });

  async function build() {
    if (!recipeId || stage === "building" || stage === "enriching") return;
    error = null;
    detail = "";
    enrichStarted = false;
    enrichUnlisten?.();
    enrichUnlisten = null;
    stage = "building";
    try {
      await installCorpus(recipeId);
    } catch (e) {
      stage = "failed";
      error = typeof e === "string" ? e : String(e);
    }
  }

  async function startEnrich(id: string) {
    stage = "enriching";
    try {
      // Bridge: scaffold the atlas config from the freshly-installed index
      // before the build (ingest didn't write one for a text atlas recipe).
      detail = "preparing atlas (reading installed index)…";
      const pipeline = await recipeEnrichInitFromCorpus(id);
      detail = `starting ${pipeline} enrichment…`;
      const handle = await enrichBuildAsync(id, null, null);
      enrichUnlisten = await listen<EnrichProgress>(handle.channel, (evt) => {
        const e = evt.payload;
        if (e.kind === "step_start") {
          detail = `enriching: ${e.step} (${e.ordinal}/${e.total})`;
        } else if (e.kind === "complete") {
          stage = "done";
          detail = `enriched — ${e.steps_completed} steps`;
          enrichUnlisten?.();
          enrichUnlisten = null;
          void loadStarters();
        } else if (e.kind === "step_failed" || e.kind === "aborted") {
          stage = "failed";
          error = `enrichment ${e.kind.replace("_", " ")}`;
          enrichUnlisten?.();
          enrichUnlisten = null;
        }
      });
    } catch (e) {
      stage = "failed";
      error = typeof e === "string" ? e : String(e);
    }
  }

  let label = $derived(enrichmentReady ? "Build & enrich" : "Build corpus");
  let busy = $derived(stage === "building" || stage === "enriching");
</script>

<Card title="Build & enrich">
  {#if !recipeId}
    <p class="muted">No recipe drafted yet.</p>
  {:else}
    <div class="actions">
      <button
        type="button"
        class="run"
        onclick={build}
        disabled={busy}
        data-testid="build-and-enrich"
      >
        {busy ? "working…" : label}
      </button>
      <span class="muted hint">
        Acquire → index{enrichmentReady ? " → atlas atoms" : ""}. Runs the real
        pipeline; then "Verify enrichment" checks the atoms.
      </span>
    </div>

    {#if stage === "building" && install}
      <div class="row">
        <span class="pill prog">{phaseLabel(install.phase)}</span>
        {#if install.percent > 0}<span class="muted">{install.percent}%</span>{/if}
        {#if install.message}<span class="muted">{install.message}</span>{/if}
      </div>
    {:else if stage === "enriching"}
      <div class="row">
        <span class="pill prog">enriching</span>
        <span class="muted">{detail}</span>
      </div>
    {:else if stage === "done"}
      <div class="row">
        <span class="pill ok">done</span>
        <span class="muted">{detail || "corpus built + enriched"}</span>
      </div>
      <!-- Land-in-use handoff: the corpus is built + installed, so a
           question mined from its atlas grounds in it the moment chat
           retrieves. Chips seed + navigate; "Open in chat" just
           navigates (always available, even with no mined questions). -->
      {#if onUseInChat || onOpenChat}
        <div class="use-corpus" data-testid="use-corpus">
          {#if starters.length > 0 && onUseInChat}
            <StarterChips
              questions={starters}
              heading="Use this corpus"
              subheading="Pick a question to drop into a grounded conversation."
              onPick={(q) => onUseInChat?.(q)}
            />
          {/if}
          {#if onOpenChat}
            <button
              type="button"
              class="open-chat"
              onclick={() => onOpenChat?.()}
              data-testid="open-in-chat"
            >
              Open in chat →
            </button>
          {/if}
        </div>
      {/if}
    {:else if stage === "failed"}
      <pre class="err-text">{error}</pre>
    {/if}
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    flex-wrap: wrap;
  }
  .run {
    background: var(--bg-elevated);
    border: 1px solid var(--border, #2a2c33);
    color: var(--fg, #e6e6e8);
    font-size: 0.78rem;
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
  }
  .run:hover:not(:disabled) {
    border-color: var(--growth, #4caf82);
  }
  .run:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .hint {
    font-size: 0.74rem;
  }
  .row {
    display: flex;
    gap: 0.6rem;
    align-items: baseline;
    font-size: 0.82rem;
    margin-top: 0.6rem;
  }
  .use-corpus {
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    margin-top: 0.7rem;
    padding-top: 0.7rem;
    border-top: 1px solid var(--border, #2a2c33);
  }
  .open-chat {
    align-self: flex-start;
    background: var(--accent-glow, color-mix(in srgb, var(--accent) 12%, transparent));
    border: 1px solid var(--accent, #c4a46a);
    color: var(--accent-light, #dfc068);
    font-size: 0.8rem;
    padding: 5px 14px;
    border-radius: 4px;
    cursor: pointer;
    transition: background 160ms ease, border-color 160ms ease;
  }
  .open-chat:hover {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .pill {
    text-transform: uppercase;
    font-size: 0.7rem;
    padding: 1px 8px;
    border-radius: 999px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .pill.ok {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .pill.prog {
    background: var(--amber-flash);
    color: var(--amber);
  }
  .err-text {
    margin: 0.5rem 0 0;
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.78rem;
    line-height: 1.4;
    color: var(--coral, #e06c75);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>

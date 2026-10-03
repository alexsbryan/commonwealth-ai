<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Glassbox card: the deterministic authoring-harness verdict ladder.
  //
  // Runs the REAL pipeline (Acquire→Extract→Filter→Chunk→Index) over a
  // frozen sample and shows a Pass / Fail / Warn verdict PER stage, with
  // the failing items shown — not summarized. Mirrors the CLI `recipe
  // test` ladder. The one networked step (freezing the sample) happens on
  // first run; thereafter it's offline + model-free. Enrichment (rung 6)
  // pays the model and is a separate, daemon-backed path.
  import Card from "./Card.svelte";
  import { recipeRunHarness } from "../../api";
  import type { HarnessRunCard, HarnessVerdict } from "../../types";

  let {
    recipePath,
    sampleSize = 15,
  }: {
    recipePath: string | null;
    sampleSize?: number;
  } = $props();

  let running = $state(false);
  let result = $state<HarnessRunCard | null>(null);
  let error = $state<string | null>(null);
  let expanded = $state<Set<string>>(new Set());

  async function run(enrich = false) {
    if (!recipePath || running) return;
    running = true;
    error = null;
    try {
      result = await recipeRunHarness(recipePath, sampleSize, enrich);
    } catch (e) {
      error = typeof e === "string" ? e : String(e);
      result = null;
    } finally {
      running = false;
    }
  }

  function toggle(key: string) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expanded = next;
  }

  function pillClass(s: HarnessVerdict["status"]): string {
    return s === "pass" ? "ok" : s === "fail" ? "fail" : "warn";
  }
</script>

<Card
  title="Authoring harness"
  counter={result ? (result.green ? "green" : "red") : null}
>
  {#if !recipePath}
    <p class="muted">No recipe drafted yet.</p>
  {:else}
    <div class="actions">
      <button
        type="button"
        class="run"
        onclick={() => run(false)}
        disabled={running}
        data-testid="harness-run"
      >
        {running ? "running…" : "Run harness"}
      </button>
      <button
        type="button"
        class="run"
        onclick={() => run(true)}
        disabled={running}
        data-testid="harness-run-enrich"
      >
        Run + enrich
      </button>
      <span class="muted hint">
        Acquire → Extract → Filter → Chunk → Index · model-free. “+ enrich”
        also verifies the installed corpus's atom integrity (rung 6).
      </span>
    </div>

    {#if error}
      <pre class="err-text">{error}</pre>
    {:else if result}
      <div class="row verdict">
        <span class="pill {result.green ? 'ok' : 'fail'}">
          {result.green ? "green" : "red"}
        </span>
        <span class="muted">
          ❄ {result.frozen_docs} frozen docs{result.frozen_captured_now
            ? " (just captured)"
            : ""}
        </span>
      </div>

      <ul class="ladder">
        {#each result.run.stages as stage}
          <li class="stage">
            <div class="stage-name">{stage.stage}</div>
            <ul class="verdicts">
              {#each stage.verdicts as v, vi}
                {@const key = `${stage.stage}:${vi}`}
                <li class="verdict-row">
                  <div class="vline">
                    <span class="pill {pillClass(v.status)}"
                      >{v.status}</span
                    >
                    <span class="observed">{v.observed}</span>
                    {#if v.evidence.length > 0}
                      <button
                        type="button"
                        class="ev-toggle"
                        onclick={() => toggle(key)}
                      >
                        {expanded.has(key)
                          ? "hide"
                          : `${v.evidence.length} item${v.evidence.length === 1 ? "" : "s"}`}
                      </button>
                    {/if}
                  </div>
                  <div class="expected">expected: {v.expected}</div>
                  {#if expanded.has(key)}
                    <ul class="evidence">
                      {#each v.evidence as ev}
                        <li>
                          <span class="locus">{ev.locus.kind} {ev.locus.id}</span
                          > — <span class="excerpt">{ev.excerpt}</span>
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </li>
              {/each}
            </ul>
          </li>
        {/each}
      </ul>
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
  }
  .verdict {
    margin-top: 0.6rem;
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
  .pill.fail {
    background: var(--coral-dim);
    color: var(--coral);
  }
  .pill.warn {
    background: var(--amber-flash);
    color: var(--amber);
  }
  .ladder {
    list-style: none;
    margin: 0.6rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .stage-name {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted, #8a8c93);
    font-weight: 600;
    margin-bottom: 0.25rem;
  }
  .verdicts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .vline {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .observed {
    font-size: 0.8rem;
    color: var(--fg, #e6e6e8);
  }
  .expected {
    font-size: 0.72rem;
    color: var(--muted, #8a8c93);
    margin-top: 1px;
  }
  .ev-toggle {
    background: transparent;
    border: 1px solid var(--border, #2a2c33);
    color: var(--muted, #8a8c93);
    font-size: 0.68rem;
    padding: 1px 7px;
    border-radius: 4px;
    cursor: pointer;
  }
  .ev-toggle:hover {
    color: var(--fg, #e6e6e8);
  }
  .evidence {
    list-style: none;
    margin: 0.35rem 0 0;
    padding: 0.4rem 0.5rem;
    background: color-mix(in srgb, var(--coral) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--coral) 25%, transparent);
    border-radius: 4px;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.75rem;
  }
  .locus {
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    color: var(--amber, #ba7517);
  }
  .excerpt {
    color: var(--fg, #e6e6e8);
    white-space: pre-wrap;
    word-break: break-word;
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

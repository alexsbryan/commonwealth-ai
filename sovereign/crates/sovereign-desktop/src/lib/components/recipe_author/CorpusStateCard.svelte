<script lang="ts">
  // Snapshot of where the corpus stands: recipe id, on-disk path,
  // last test status. Pure read; the actual recipe TOML lives in the
  // technical detail drawer.
  import Card from "./Card.svelte";

  let {
    recipeId,
    recipePath,
    lastTestStatus,
    lastTestAt,
  }: {
    recipeId: string | null;
    recipePath: string | null;
    lastTestStatus: string | null;
    lastTestAt: string | null;
  } = $props();

  function fmtTime(ts: string | null): string {
    if (!ts) return "—";
    try {
      return new Date(ts).toLocaleString();
    } catch {
      return ts;
    }
  }
</script>

<Card title="Corpus state">
  <dl>
    <dt>Recipe</dt>
    <dd>
      {#if recipeId}
        <code>{recipeId}</code>
      {:else}
        <span class="muted">not drafted yet</span>
      {/if}
    </dd>
    <dt>Last test</dt>
    <dd>
      {#if lastTestStatus}
        <span
          class="status"
          class:pass={lastTestStatus === "pass"}
          class:fail={lastTestStatus === "fail"}>{lastTestStatus}</span
        >
        <span class="muted">at {fmtTime(lastTestAt)}</span>
      {:else}
        <span class="muted">no test runs yet</span>
      {/if}
    </dd>
    {#if recipePath}
      <dt>Path</dt>
      <dd><code class="path" title={recipePath}>{recipePath}</code></dd>
    {/if}
  </dl>
</Card>

<style>
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: 88px 1fr;
    gap: 0.3rem 0.6rem;
    font-size: 0.82rem;
  }
  dt {
    color: var(--muted, #8a8c93);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding-top: 2px;
  }
  dd {
    margin: 0;
  }
  code {
    font-family: ui-monospace, monospace;
    font-size: 0.78rem;
  }
  .path {
    word-break: break-all;
    color: var(--muted, #8a8c93);
  }
  .muted {
    color: var(--muted, #8a8c93);
  }
  .status {
    text-transform: uppercase;
    font-size: 0.68rem;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--bg-elevated);
    margin-right: 0.3rem;
  }
  .status.pass {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .status.fail {
    background: var(--coral-dim);
    color: var(--coral);
  }
</style>

<script lang="ts">
  // Research findings the agent has captured during the session.
  // Each finding is one `kind=research_finding` note carrying a
  // claim + source URL + authority tag in its payload.
  import Card from "./Card.svelte";
  import type { DashboardNoteEntry } from "../../types";

  let { findings }: { findings: DashboardNoteEntry[] } = $props();

  function field(p: unknown, key: string): string | null {
    if (p && typeof p === "object" && key in p) {
      const v = (p as Record<string, unknown>)[key];
      return typeof v === "string" ? v : null;
    }
    return null;
  }
</script>

<Card title="Research log" counter={findings.length}>
  {#if findings.length === 0}
    <p class="muted">No findings captured yet.</p>
  {:else}
    <ul>
      {#each findings as f (f.id)}
        <li>
          <div class="row-head">
            {#if field(f.payload, "authority")}
              <span
                class="auth"
                class:authoritative={field(f.payload, "authority") === "authoritative"}
                class:secondary={field(f.payload, "authority") === "secondary"}
                class:unverified={field(f.payload, "authority") === "unverified"}
                >{field(f.payload, "authority")}</span
              >
            {/if}
            <span class="when">{f.created_at.slice(0, 10)}</span>
          </div>
          <p class="claim">{f.content}</p>
          {#if field(f.payload, "source_url")}
            <p class="src">
              <code>{field(f.payload, "source_url")}</code>
            </p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
  }
  li {
    border-left: 2px solid rgba(140, 220, 200, 0.4);
    padding: 0.1rem 0 0.1rem 0.55rem;
  }
  .row-head {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
    font-size: 0.7rem;
  }
  .auth {
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 1px 7px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.05);
  }
  .auth.authoritative {
    background: rgba(120, 220, 160, 0.18);
    color: #b9f0c9;
  }
  .auth.secondary {
    background: rgba(120, 200, 240, 0.15);
    color: #c0e0f0;
  }
  .auth.unverified {
    background: rgba(240, 180, 100, 0.18);
    color: #f0c98c;
  }
  .when {
    margin-left: auto;
    color: var(--muted, #8a8c93);
  }
  .claim {
    margin: 0.2rem 0 0.15rem;
    font-size: 0.82rem;
    line-height: 1.35;
  }
  .src {
    margin: 0;
  }
  .src code {
    font-size: 0.72rem;
    color: var(--muted, #8a8c93);
    word-break: break-all;
  }
</style>

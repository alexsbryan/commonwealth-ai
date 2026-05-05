<script lang="ts">
  // Shared visual frame for every dashboard card: title + optional
  // counter pill + slot for body. Keeps each leaf card pure and
  // visually consistent without duplicating the chrome 9 times.
  import type { Snippet } from "svelte";

  let {
    title,
    counter,
    children,
  }: {
    title: string;
    counter?: number | string | null;
    children: Snippet;
  } = $props();
</script>

<section class="card">
  <header>
    <h3>{title}</h3>
    {#if counter !== undefined && counter !== null && counter !== ""}
      <span class="counter">{counter}</span>
    {/if}
  </header>
  <div class="body">
    {@render children()}
  </div>
</section>

<style>
  .card {
    background: rgba(255, 255, 255, 0.025);
    border: 1px solid var(--border, #2a2c33);
    border-radius: 6px;
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.4rem 0.7rem;
    background: rgba(255, 255, 255, 0.025);
    border-bottom: 1px solid var(--border, #2a2c33);
  }
  h3 {
    margin: 0;
    font-size: 0.78rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted-bright, #b8bac1);
  }
  .counter {
    font-size: 0.72rem;
    background: rgba(255, 255, 255, 0.07);
    color: inherit;
    padding: 1px 8px;
    border-radius: 999px;
  }
  .body {
    padding: 0.5rem 0.7rem 0.6rem;
    font-size: 0.85rem;
    color: var(--fg, #e6e6e8);
  }
</style>

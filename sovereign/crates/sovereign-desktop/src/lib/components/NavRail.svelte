<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { fade } from "svelte/transition";

  type RailMode = "home" | "chat" | "library" | "inner_work" | "workshop" | "settings";

  interface Props {
    active: RailMode;
    onNavigate: (mode: RailMode) => void;
  }

  let { active, onNavigate }: Props = $props();

  let hoveredIdx: number | null = $state(null);

  // Rail order: Ask · Library · Reflect · Workshop · Settings. Labels are
  // verbs/nouns a newcomer can predict (the evocative "Outer/Inner Work" names
  // can return as hover taglines in the later copy sweep). Library is the
  // knowledge home — per-notebook Ask + Explore replace the old top-level
  // Atlas rail (the atlas surface lives on inside a notebook's Explore tab and
  // as a reading deep-link target). The Workshop holds the maker surfaces
  // (Build/Run) — always present, no opt-in flag. Static now that nothing is
  // gated.
  const marks: { id: RailMode; label: string; testid: string }[] = [
    { id: "home", label: "Home", testid: "nav-home" },
    { id: "chat", label: "Ask", testid: "nav-ask" },
    { id: "library", label: "Library", testid: "nav-library" },
    { id: "inner_work", label: "Reflect", testid: "nav-reflect" },
    { id: "workshop", label: "Workshop", testid: "nav-workshop" },
    { id: "settings", label: "Settings", testid: "nav-settings" },
  ];
</script>

<nav
  class="nav-rail"
  class:mode-inner={active === "inner_work"}
  aria-label="Main navigation"
>
  {#each marks as mark, i}
    <div class="mark-wrap">
      <button
        class="mark"
        class:active={active === mark.id}
        onclick={() => onNavigate(mark.id)}
        aria-label={mark.label}
        aria-current={active === mark.id ? "page" : undefined}
        data-testid={mark.testid}
        onmouseenter={() => (hoveredIdx = i)}
        onmouseleave={() => (hoveredIdx = null)}
      >
        {#if mark.id === "home"}
          <!-- Lucide: house — the hub / landing -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/>
            <path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>
          </svg>
        {:else if mark.id === "chat"}
          <!-- Lucide: message-square — Ask -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
          </svg>
        {:else if mark.id === "inner_work"}
          <!-- Lucide: moon — calm and introspective -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401"/>
          </svg>
        {:else if mark.id === "library"}
          <!-- Lucide: library — the knowledge home -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="m16 6 4 14"/>
            <path d="M12 6v14"/>
            <path d="M8 8v12"/>
            <path d="M4 4v16"/>
          </svg>
        {:else if mark.id === "workshop"}
          <!-- Lucide: wrench — the maker surfaces (Build / Run) -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/>
          </svg>
        {:else}
          <!-- Lucide: settings (cog) -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"/>
            <circle cx="12" cy="12" r="3"/>
          </svg>
        {/if}
      </button>
      {#if hoveredIdx === i}
        <span class="mark-label" aria-hidden="true" transition:fade={{ duration: 150 }}>
          {mark.label}
        </span>
      {/if}
    </div>
  {/each}
</nav>

<style>
  .nav-rail {
    width: 60px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-top: 28px;
    gap: 2px;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    transition: background 350ms ease, border-color 350ms ease;
    position: relative;
    z-index: 10;
  }

  /* In inner work mode the rail dissolves into the page's light field */
  .nav-rail.mode-inner {
    background: oklch(98% 0.006 250);
    border-right-color: oklch(75% 0.008 250 / 0.35);
  }

  .mark-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }

  .mark {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    position: relative;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    border-radius: var(--radius);
    transition: color 200ms ease;
  }

  .mark:hover {
    color: var(--text-secondary);
  }

  .mark.active {
    color: var(--accent);
  }

  /* Thin vertical line in the left gutter for the active mark */
  .mark.active::before {
    content: "";
    position: absolute;
    left: -12px;
    top: 22%;
    height: 56%;
    width: 2px;
    background: var(--accent);
    border-radius: 1px;
  }

  /* In inner work mode swap accent for dark ink */
  .nav-rail.mode-inner .mark {
    color: oklch(70% 0.010 250 / 0.6);
  }

  .nav-rail.mode-inner .mark:hover {
    color: oklch(45% 0.012 250);
  }

  .nav-rail.mode-inner .mark.active {
    color: oklch(22% 0.015 250);
  }

  .nav-rail.mode-inner .mark.active::before {
    background: oklch(22% 0.015 250);
  }

  /* Floating label — appears to the right of the hovered mark.
     Uses position:absolute on the mark-wrap so it escapes the rail's
     own bounds. pointer-events: none so it doesn't block the cursor. */
  .mark-label {
    position: absolute;
    left: calc(100% + 8px);
    top: 50%;
    transform: translateY(-50%);
    font-family: var(--font-sans);
    font-size: 0.68rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    color: var(--text-secondary);
    background: var(--bg-elevated);
    border: 1px solid var(--border-mid);
    padding: 4px 9px;
    border-radius: var(--radius);
    white-space: nowrap;
    pointer-events: none;
    z-index: 100;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
  }

  /* In inner work mode the label uses the page's ink palette */
  .nav-rail.mode-inner .mark-label {
    color: oklch(22% 0.015 250);
    background: oklch(96% 0.005 250);
    border-color: oklch(75% 0.008 250 / 0.4);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.08);
  }
</style>

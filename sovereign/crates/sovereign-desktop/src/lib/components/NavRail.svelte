<script lang="ts">
  import { fade } from "svelte/transition";

  type RailMode = "chat" | "inner_work" | "settings";

  interface Props {
    active: RailMode;
    onNavigate: (mode: RailMode) => void;
  }

  let { active, onNavigate }: Props = $props();

  let hoveredIdx: number | null = $state(null);

  const marks: { id: RailMode; label: string; testid: string }[] = [
    { id: "chat", label: "Outer Work", testid: "nav-chat" },
    { id: "inner_work", label: "Inner Work", testid: "open-inner-work" },
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
        {#if mark.id === "chat"}
          <!-- Lucide: briefcase -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M16 20V4a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16"/>
            <rect width="20" height="14" x="2" y="6" rx="2"/>
          </svg>
        {:else if mark.id === "inner_work"}
          <!-- Lucide: moon — calm and introspective -->
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401"/>
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

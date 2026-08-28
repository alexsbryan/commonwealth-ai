<script lang="ts">
  /**
   * The mesh switcher: every mesh this node has joined, active one marked.
   *
   * Extracted rather than added to MeshSettings.svelte, which is already past
   * the 1200-line split threshold (ARCH §3.1). The parent owns the join-link
   * paste flow and passes it in as a snippet, so there is still exactly one
   * implementation of "paste an invite" in the app.
   *
   * A parked mesh keeps its full roster and its mesh secret on disk, so
   * switching back is a resume — no invite redeemed, no founder involved,
   * and an expired invite is irrelevant. That is why a parked row offers
   * "Switch" and not "Rejoin".
   */
  import type { KnownMesh } from "../types";
  import type { Snippet } from "svelte";

  let {
    meshes,
    switching,
    onSwitch,
    joinBox,
  }: {
    meshes: KnownMesh[];
    /** mesh_id currently being switched to, or null. */
    switching: string | null;
    onSwitch: (mesh: KnownMesh) => void;
    joinBox: Snippet<[string]>;
  } = $props();

  /** Only worth showing the list when there is a choice to make. */
  const hasChoice = $derived(meshes.length > 1);

  function agoLabel(unix: number): string {
    if (!unix) return "never seen";
    const secs = Math.floor(Date.now() / 1000) - unix;
    if (secs < 90) return "seen just now";
    if (secs < 3600) return `seen ${Math.floor(secs / 60)}m ago`;
    if (secs < 86400) return `seen ${Math.floor(secs / 3600)}h ago`;
    return `seen ${Math.floor(secs / 86400)}d ago`;
  }
</script>

{#if hasChoice}
  <div class="mesh-list">
    <p class="section-label">Meshes</p>
    <ul>
      {#each meshes as m (m.mesh_id)}
        <li class="mesh-row" class:active={m.is_active}>
          <span class="dot" class:on={m.is_active}></span>
          <span class="name">{m.name}</span>
          <span class="meta">
            {m.members_total}
            {m.members_total === 1 ? "member" : "members"}
            {#if !m.is_active}
              · {agoLabel(m.last_seen_unix)}
            {/if}
          </span>
          {#if m.is_active}
            <span class="badge">active</span>
          {:else}
            <button
              class="switch-btn"
              disabled={switching !== null}
              onclick={() => onSwitch(m)}
            >
              {switching === m.mesh_id ? "Switching…" : "Switch"}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
    <p class="hint">
      Switching parks the current mesh — peers there see you go offline, not
      leave, so you can switch back without a new invite.
    </p>
  </div>
{/if}

{@render joinBox(hasChoice ? "Join another mesh" : "Join a mesh")}

<style>
  .mesh-list {
    margin-bottom: 1.25rem;
  }
  .mesh-list ul {
    list-style: none;
    margin: 0.5rem 0 0.5rem;
    padding: 0;
  }
  .mesh-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.65rem;
    border-radius: 6px;
  }
  .mesh-row.active {
    background: var(--surface-2, rgba(127, 127, 127, 0.08));
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted, #8a8a8a);
    flex: none;
  }
  .dot.on {
    background: var(--success, #3fb950);
  }
  .name {
    font-weight: 600;
  }
  .meta {
    color: var(--muted, #8a8a8a);
    font-size: 0.85em;
    margin-left: auto;
  }
  .badge {
    font-size: 0.75em;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted, #8a8a8a);
  }
  .switch-btn {
    font-size: 0.85em;
    padding: 0.2rem 0.6rem;
  }
  .hint {
    font-size: 0.85em;
    color: var(--muted, #8a8a8a);
    margin: 0;
  }
</style>

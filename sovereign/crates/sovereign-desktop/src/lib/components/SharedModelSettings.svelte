<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getConfig, saveConfig, getSharedModelStatus } from "../api";
  import type { DesktopConfig, SharedModelStatus } from "../types";

  // The "Shared model" surface: a fleet of desktops sharing ONE distributed
  // model (e.g. GLM-5.2) as their collective primary. This node picks a role —
  // Use it (route my chats in), Lend my GPU (hold a shard), or Run it here
  // (assemble + serve for everyone) — and sees the cluster's health at a glance.
  //
  // Svelte 5 runes: type via the `$state<T>()` generic, NOT a
  // `let x: T = $state(...)` annotation (the latter collapses to `never`).
  let config = $state<DesktopConfig | null>(null);
  let status = $state<SharedModelStatus | null>(null);
  let busy = $state(false);
  let errorMessage = $state<string | null>(null);
  let showHostConsent = $state(false);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  type Role = "consumer" | "anchor" | "host";

  const ROLES: { value: Role; label: string; blurb: string }[] = [
    {
      value: "consumer",
      label: "Use it",
      blurb:
        "Route my chats into the shared model. I hold none of it; when it's unavailable I fall back to my local model.",
    },
    {
      value: "anchor",
      label: "Lend my GPU",
      blurb:
        "Hold a shard of the model so the fleet can run it. My machine stays on and reachable while I'm online.",
    },
    {
      value: "host",
      label: "Run it here",
      blurb:
        "This machine assembles and serves the model for everyone — or fails over to another anchor if it drops. The biggest commitment.",
    },
  ];

  const role = $derived<Role>((config?.shared_model_role as Role) ?? "consumer");
  const activeBlurb = $derived(
    ROLES.find((r) => r.value === role)?.blurb ?? "",
  );

  // Cluster-health chip text, or null when this node isn't in a fleet yet.
  const chip = $derived.by(() => {
    const s = status;
    if (!s?.configured) return null;
    const state = s.available ? "available" : `forming ${s.eligible_anchors}/${s.quorum_anchors}`;
    const anchors = s.available ? ` · ${s.eligible_anchors}/${s.quorum_anchors} anchors` : "";
    return `${s.model_id ?? "shared model"} · ${state}${anchors}`;
  });

  // A consumer whose shared model is forming is answering from its local model.
  const degraded = $derived(
    !!status?.configured && role === "consumer" && !status.available,
  );

  async function refresh() {
    const [cfg, st] = await Promise.allSettled([
      getConfig(),
      getSharedModelStatus(),
    ]);
    if (cfg.status === "fulfilled") config = cfg.value;
    if (st.status === "fulfilled") status = st.value;
  }

  async function applyRole(next: Role) {
    if (!config || busy) return;
    const prev = (config.shared_model_role as Role) ?? "consumer";
    if (prev === next) return;
    config.shared_model_role = next;
    busy = true;
    errorMessage = null;
    try {
      await saveConfig(config);
      await refresh();
    } catch (e) {
      config.shared_model_role = prev; // revert the optimistic change
      errorMessage = `Couldn't apply role: ${e}`;
    } finally {
      busy = false;
    }
  }

  function chooseRole(next: Role) {
    // Hosting is the heaviest commitment — confirm before flipping to it.
    if (next === "host" && role !== "host") {
      showHostConsent = true;
      return;
    }
    applyRole(next);
  }

  function confirmHost() {
    showHostConsent = false;
    applyRole("host");
  }

  onMount(() => {
    refresh();
    pollHandle = setInterval(refresh, 5000);
  });
  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });
</script>

<section class="shared-model">
  <p class="hint">
    Run a model none of you could run alone. The fleet holds one shared
    instance; pick how this machine takes part. Throughput is a shared,
    measured-pace resource — occasional access to something big, scheduled
    fairly, not a snappy daily driver.
  </p>

  {#if chip}
    <p class="state" class:state-available={status?.available} class:state-forming={!status?.available}>
      {chip}{status?.is_host ? " · hosting here" : ""}
    </p>
  {/if}

  {#if degraded}
    <p class="state state-degraded" role="status">
      Shared model is forming — answering from your local model until the fleet
      reaches {status?.quorum_anchors} anchors.
    </p>
  {/if}

  <h3 class="h3">This machine's role</h3>
  <div class="presets">
    {#each ROLES as r (r.value)}
      <button
        class="preset"
        class:active={role === r.value}
        onclick={() => chooseRole(r.value)}
        disabled={busy || config === null}
      >
        {r.label}
      </button>
    {/each}
  </div>
  <p class="hint role-blurb">{activeBlurb}</p>

  {#if !status?.configured}
    <p class="hint subtle">
      No shared model is set for this fleet yet. Join a mesh and set one up, and
      this panel will show its health.
    </p>
  {/if}

  {#if errorMessage}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}
</section>

{#if showHostConsent}
  <div
    class="modal-backdrop"
    onclick={() => (showHostConsent = false)}
    onkeydown={(e) => e.key === "Escape" && (showHostConsent = false)}
    role="presentation"
  >
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label="Confirm hosting"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <h3 class="modal-title">Run the shared model here?</h3>
      <p class="modal-body">
        Hosting dedicates this machine's GPU and memory to assembling and
        serving the shared model for the whole fleet while you're online. It
        should stay on and reachable. If it drops, another anchor takes over
        automatically — but until then, everyone's access pauses.
      </p>
      <div class="modal-actions">
        <button class="action" onclick={() => (showHostConsent = false)}>Cancel</button>
        <button class="action action-primary" onclick={confirmHost}>Host the model</button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* Lavender Court substrate — matches every other Settings section. */
  .shared-model {
    font-family: var(--font-sans);
    color: var(--text-secondary);
    -webkit-font-smoothing: antialiased;
  }

  .h3 {
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 28px 0 8px;
    letter-spacing: -0.005em;
  }

  .hint {
    font-size: 0.88rem;
    color: var(--text-muted);
    margin: 0 0 14px;
    line-height: 1.5;
    max-width: 540px;
  }

  .role-blurb {
    margin-top: 10px;
    margin-bottom: 0;
  }

  .subtle {
    color: var(--text-muted);
    font-style: italic;
  }

  /* ── Role presets ── */
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .preset,
  .action {
    font-family: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    background: none;
    border: 1px solid var(--border-mid);
    padding: 7px 14px;
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 160ms ease, background 160ms ease, color 160ms ease;
  }

  .preset:hover:not(:disabled),
  .action:hover:not(:disabled) {
    border-color: var(--border-bright);
    background: var(--bg-surface);
    color: var(--text-primary);
  }

  .preset.active {
    border-color: var(--accent);
    background: var(--accent-dim);
    color: var(--accent-light);
  }

  .preset:disabled,
  .action:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  /* ── Cluster-health chip + banners ── */
  .state {
    font-size: 0.88rem;
    margin: 12px 0;
    padding: 8px 12px;
    border-radius: var(--radius);
    font-variant-numeric: tabular-nums;
  }

  .state-available {
    background: rgba(121, 196, 120, 0.08);
    color: var(--success);
    border: 1px solid rgba(121, 196, 120, 0.32);
  }

  .state-forming {
    background: rgba(201, 168, 76, 0.08);
    color: var(--warning);
    border: 1px solid rgba(201, 168, 76, 0.32);
  }

  .state-degraded {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border: 1px solid rgba(155, 135, 196, 0.32);
  }

  .error {
    color: var(--error);
    font-size: 0.88rem;
    margin: 12px 0 0;
  }

  /* ── Host consent modal ── */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .modal {
    width: min(440px, 92vw);
    background: var(--bg-surface);
    border: 1px solid var(--border-bright);
    border-radius: var(--radius-lg);
    padding: 22px 24px;
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.5);
  }

  .modal-title {
    font-size: 1.02rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 0 0 10px;
  }

  .modal-body {
    font-size: 0.9rem;
    color: var(--text-secondary);
    line-height: 1.55;
    margin: 0 0 20px;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .action-primary {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--text-on-accent);
    font-weight: 600;
  }

  .action-primary:hover:not(:disabled) {
    background: var(--accent-light);
    border-color: var(--accent-light);
    color: var(--text-on-accent);
  }
</style>

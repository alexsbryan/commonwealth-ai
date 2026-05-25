<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getContributionStatus,
    getRecentContributions,
    pauseContributions,
    resumeContributions,
    setContributionCeiling,
    type ContributionStatus,
    type LedgerEventDto,
  } from "../api";

  // Polls the daemon's /internal/contribution/status every 5s and
  // renders the W3 settings surface: ceiling slider, pause status +
  // duration buttons, live served-events feed. Mirrors the tray
  // menu's pause submenu (15m / 1h / Until I resume) so users have
  // the same controls from either entry point.

  let status: ContributionStatus | null = $state(null);
  let events: LedgerEventDto[] = $state([]);
  let busy = $state(false);
  let errorMessage: string | null = $state(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // Ceiling buckets — the same coarse model the W4 consent uses:
  // 0 = decline, 1 = "share a little" (one concurrent peer request,
  // ~25%), 2/3 = "share more", unlimited = no cap. The slider's
  // values map to these buckets explicitly so the user's intent is
  // preserved across rebuilds of the rendering.
  type CeilingPreset = 0 | 1 | 2 | 3 | -1; // -1 = unlimited sentinel
  const PRESETS: { value: CeilingPreset; label: string; hint: string }[] = [
    { value: 0,  label: "Off",        hint: "Don't share with the mesh" },
    { value: 1,  label: "A little",   hint: "Up to 1 peer request at a time" },
    { value: 2,  label: "Some",       hint: "Up to 2 peer requests at a time" },
    { value: 3,  label: "More",       hint: "Up to 3 peer requests at a time" },
    { value: -1, label: "Unlimited",  hint: "No cap — full machine to peers when idle" },
  ];

  function ceilingPreset(s: ContributionStatus): CeilingPreset {
    if (s.ceiling === 0) return 0;
    if (s.ceiling >= 9_000_000_000_000_000) return -1;
    if (s.ceiling >= 3) return 3;
    if (s.ceiling === 2) return 2;
    return 1;
  }

  async function refresh() {
    try {
      status = await getContributionStatus();
      errorMessage = null;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    try {
      events = await getRecentContributions(10);
    } catch {
      // Recent-events feed is decorative; failure shouldn't blank
      // the rest of the panel.
    }
  }

  async function chooseCeiling(preset: CeilingPreset) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      // `null` to setContributionCeiling means unlimited.
      const max = preset === -1 ? null : preset;
      status = await setContributionCeiling(max);
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function pause(durationSecs: number) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      // 0 from the "Until I resume" preset maps to a far-future
      // expiry (365 days). The render logic in Rust's tray code
      // recognises the magic ceiling and shows "Paused (until I
      // resume)" instead of a year-long countdown.
      const secs = durationSecs === 0 ? 365 * 24 * 3600 : durationSecs;
      status = await pauseContributions(secs);
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function resume() {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      status = await resumeContributions();
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function formatPauseRemaining(secs: number): string {
    if (secs >= 365 * 24 * 3600 - 600) return "until you resume";
    if (secs >= 3600) {
      const h = Math.floor(secs / 3600);
      const m = Math.round((secs % 3600) / 60);
      return m === 0 ? `${h}h` : `${h}h ${m}m`;
    }
    if (secs >= 60) return `${Math.ceil(secs / 60)} min`;
    return `${secs}s`;
  }

  function summariseEvent(e: LedgerEventDto): string {
    const k = (e.kind?.type as string | undefined) ?? "Event";
    if (k === "KnowledgeQueryServed") {
      const corpus = String(e.kind.corpus_id ?? "");
      const n = Number(e.kind.chunks_returned ?? 0);
      return `Knowledge query served — ${corpus} (${n} chunks)`;
    }
    if (k === "InferenceServed") {
      const model = String(e.kind.model_id ?? "");
      const tokens = Number(e.kind.tokens_generated ?? 0);
      return `Inference served — ${model} (${tokens} tokens)`;
    }
    if (k === "ShardTransferred") {
      const bytes = Number(e.kind.bytes ?? 0);
      const mb = (bytes / 1048576).toFixed(1);
      return `Shard transferred — ${mb} MB`;
    }
    return k;
  }

  function relativeTime(unixSecs: number): string {
    const now = Math.floor(Date.now() / 1000);
    const delta = now - unixSecs;
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  onMount(() => {
    void refresh();
    pollHandle = setInterval(refresh, 5000);
  });

  onDestroy(() => {
    if (pollHandle !== null) clearInterval(pollHandle);
  });

  let currentPreset: CeilingPreset = $derived.by(() => {
    const s = status;
    return s === null ? 0 : ceilingPreset(s);
  });

  let paused: boolean = $derived.by(() => {
    const s = status;
    return s !== null && s.pause_remaining_secs !== null;
  });

  let yielding: boolean = $derived.by(() => {
    const s = status;
    return s !== null && s.yielding_secs_remaining !== null;
  });
</script>

<section class="sharing">
  <h3 class="h3">How much to share</h3>
  <p class="hint">
    When your machine has spare cycles, peers can use them. Pick a
    ceiling that feels right — change it any time.
  </p>
  <div class="presets">
    {#each PRESETS as p (p.value)}
      <button
        class="preset"
        class:active={currentPreset === p.value}
        onclick={() => chooseCeiling(p.value)}
        disabled={busy || status === null}
        title={p.hint}
      >
        {p.label}
      </button>
    {/each}
  </div>

  <h3 class="h3">Pause sharing</h3>
  {#if paused && status}
    <p class="state state-paused">
      Paused — resumes in {formatPauseRemaining(status.pause_remaining_secs!)}
    </p>
    <div class="row">
      <button class="action" onclick={resume} disabled={busy}>
        Resume now
      </button>
    </div>
  {:else}
    <p class="hint">
      Pause for a moment when you want every cycle to yourself — recording,
      gaming, anything that hates a busy machine.
    </p>
    <div class="row">
      <button class="action" onclick={() => pause(15 * 60)} disabled={busy}>
        15 minutes
      </button>
      <button class="action" onclick={() => pause(60 * 60)} disabled={busy}>
        1 hour
      </button>
      <button class="action" onclick={() => pause(0)} disabled={busy}>
        Until I resume
      </button>
    </div>
  {/if}

  {#if yielding && !paused}
    <p class="state state-yielding">
      Stepping aside for your chat — peer work paused for a moment.
    </p>
  {/if}

  {#if errorMessage}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}

  <h3 class="h3">Recent activity</h3>
  {#if events.length === 0}
    <p class="hint">No peer requests served yet.</p>
  {:else}
    <ul class="feed">
      {#each events as e (`${e.timestamp}-${e.kind?.type}`)}
        <li class="feed-item">
          <span class="feed-text">{summariseEvent(e)}</span>
          <span class="feed-time">{relativeTime(e.timestamp)}</span>
        </li>
      {/each}
    </ul>
  {/if}

  {#if status}
    <p class="meta">
      Currently serving: <strong>{status.in_flight}</strong>
      {status.in_flight === 1 ? "request" : "requests"}
    </p>
  {/if}
</section>

<style>
  /* Lavender Court substrate — matches every other section inside
     Settings. The previous off-white / dark-ink palette was an
     orphan that rendered as a light card floating in the dark
     Configuration page. */
  .sharing {
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

  .h3:first-child {
    margin-top: 0;
  }

  .hint {
    font-size: 0.88rem;
    color: var(--text-muted);
    margin: 0 0 14px;
    line-height: 1.5;
    max-width: 540px;
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .preset {
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

  .preset:hover:not(:disabled) {
    border-color: var(--border-bright);
    background: var(--bg-surface);
    color: var(--text-primary);
  }

  .preset.active {
    border-color: var(--accent);
    background: var(--accent-dim);
    color: var(--accent-light);
  }

  .preset:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .action {
    font-family: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--text-secondary);
    background: none;
    border: 1px solid var(--border-mid);
    padding: 7px 14px;
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 160ms ease, background 160ms ease, color 160ms ease;
  }

  .action:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--bg-surface);
  }

  .action:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  /* Status pills — semantic alert tints layered on the dark
     substrate. The colour cues stay (warning amber for paused,
     mesh lavender for yielding), but anchored to the design tokens
     so they coexist with the dark Settings background. */
  .state {
    font-size: 0.88rem;
    margin: 0 0 14px;
    padding: 8px 12px;
    border-radius: var(--radius);
  }

  .state-paused {
    background: rgba(201, 168, 76, 0.08);
    color: var(--warning);
    border: 1px solid rgba(201, 168, 76, 0.32);
  }

  .state-yielding {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border: 1px solid rgba(155, 135, 196, 0.32);
    margin-top: 12px;
  }

  .error {
    color: var(--error);
    font-size: 0.88rem;
    margin: 12px 0 0;
  }

  .feed {
    list-style: none;
    padding: 0;
    margin: 0 0 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--bg-secondary);
  }

  .feed-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 8px 14px;
    font-size: 0.85rem;
    border-bottom: 1px solid var(--border);
  }

  .feed-item:last-child {
    border-bottom: none;
  }

  .feed-text {
    color: var(--text-secondary);
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .feed-time {
    color: var(--text-muted);
    font-size: 0.78rem;
    flex-shrink: 0;
  }

  .meta {
    font-size: 0.85rem;
    color: var(--text-muted);
    margin: 14px 0 0;
  }
</style>

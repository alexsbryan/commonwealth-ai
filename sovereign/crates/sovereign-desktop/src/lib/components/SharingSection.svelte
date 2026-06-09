<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getActivitySummary,
    getActivityRecent,
    getChatActivity,
    getContributionStatus,
    getRecentContributions,
    pauseContributions,
    resumeContributions,
    setContributionCeiling,
    getIngestBudget,
    setIngestBudget,
    getMeshQuiesced,
    setMeshQuiesced,
    newsworthyStatus,
    type ActivitySummary,
    type ChatActivitySummary,
    type ContributionStatus,
    type ActivityEventDto,
    type LedgerEventDto,
    type NewsworthyStatus,
  } from "../api";
  import { corpusProgressStore } from "../stores/corpusProgress.svelte";

  // The glassbox "Activity & Sharing" surface. Three reads on a 5s
  // poll, plus the event-driven corpus-progress store, answer "what
  // has my daemon been doing — for me, and for the mesh?" in
  // Sovereign's own vocabulary (tokens, embeddings, chunks, queries),
  // even as a mesh of one. Below the visibility sit the controls —
  // "the reins" — that decide how hard the daemon works.

  // Svelte 5 runes: type via the `$state<T>()` generic, NOT a
  // `let x: T = $state(...)` annotation — the latter collapses to
  // `never` under svelte-check.
  let activity = $state<ActivitySummary | null>(null);
  let chat = $state<ChatActivitySummary | null>(null);
  let status = $state<ContributionStatus | null>(null);
  let feed = $state<(ActivityEventDto | LedgerEventDto)[]>([]);
  let news = $state<NewsworthyStatus | null>(null);
  let throttleFactor = $state(1.0);
  let quiesced = $state(false);

  let busy = $state(false);
  let errorMessage = $state<string | null>(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // Window for the totals. 7 days matches the daemon's default
  // activity window; kept here so a future toggle has a home.
  const WINDOW_DAYS = 7;

  // ── Peer-share ceiling presets (unchanged W3 model) ──────────
  type CeilingPreset = 0 | 1 | 2 | 3 | -1; // -1 = unlimited sentinel
  const PRESETS: { value: CeilingPreset; label: string; hint: string }[] = [
    { value: 0, label: "Off", hint: "Don't share with the mesh" },
    { value: 1, label: "A little", hint: "Up to 1 peer request at a time" },
    { value: 2, label: "Some", hint: "Up to 2 peer requests at a time" },
    { value: 3, label: "More", hint: "Up to 3 peer requests at a time" },
    { value: -1, label: "Unlimited", hint: "No cap — full machine to peers when idle" },
  ];

  // ── Background-ingest throttle presets (reused /internal/ingest/budget) ─
  const THROTTLE_PRESETS: { value: number; label: string; hint: string }[] = [
    { value: 1.0, label: "Full speed", hint: "Ingest takes every cycle it can" },
    { value: 0.75, label: "Light", hint: "75% duty cycle — barely noticeable" },
    { value: 0.5, label: "Balanced", hint: "50% — about twice as long; machine stays usable" },
    { value: 0.25, label: "Quiet", hint: "25% — hums in the background" },
  ];

  function ceilingPreset(s: ContributionStatus): CeilingPreset {
    if (s.ceiling === 0) return 0;
    if (s.ceiling >= 9_000_000_000_000_000) return -1;
    if (s.ceiling >= 3) return 3;
    if (s.ceiling === 2) return 2;
    return 1;
  }

  async function refresh() {
    // Each read is independent and decorative on failure — a daemon
    // hiccup must not blank the whole panel, so we keep last-good
    // values and only surface the contribution-status error (the one
    // the controls below act on).
    const [act, ch, st, localFeed, peerFeed, nws, budget, quiesce] =
      await Promise.allSettled([
        getActivitySummary(WINDOW_DAYS),
        getChatActivity(WINDOW_DAYS),
        getContributionStatus(),
        getActivityRecent(20),
        getRecentContributions(20),
        newsworthyStatus(),
        getIngestBudget(),
        getMeshQuiesced(),
      ]);

    // Normalize `undefined` → `null` on every assignment. A fulfilled
    // promise can still carry `undefined` (e.g. a command that returned
    // nothing), and `undefined !== null` is `true` — so guarding only
    // against `null` would let `activity.peer_…` throw. Keep last-good
    // values when a read is missing rather than blanking the panel.
    if (act.status === "fulfilled" && act.value) activity = act.value;
    if (ch.status === "fulfilled" && ch.value) chat = ch.value;
    if (st.status === "fulfilled" && st.value) {
      status = st.value;
      errorMessage = null;
    } else if (st.status === "rejected") {
      errorMessage =
        st.reason instanceof Error ? st.reason.message : String(st.reason);
    }
    if (nws.status === "fulfilled" && nws.value) news = nws.value;
    if (budget.status === "fulfilled" && budget.value)
      throttleFactor = budget.value.throttle_factor;
    if (quiesce.status === "fulfilled" && quiesce.value)
      quiesced = quiesce.value.quiesced;

    // Merge the local-activity feed and the peer-contribution feed
    // into one timeline, newest first.
    const merged: (ActivityEventDto | LedgerEventDto)[] = [];
    // Guard the spread: a fulfilled promise can carry `undefined` (an
    // unstubbed/empty read), and `...undefined` throws "not iterable".
    if (localFeed.status === "fulfilled" && Array.isArray(localFeed.value))
      merged.push(...localFeed.value);
    if (peerFeed.status === "fulfilled" && Array.isArray(peerFeed.value))
      merged.push(...peerFeed.value);
    merged.sort((a, b) => b.timestamp - a.timestamp);
    feed = merged.slice(0, 24);
  }

  async function chooseCeiling(preset: CeilingPreset) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      status = await setContributionCeiling(preset === -1 ? null : preset);
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

  async function chooseThrottle(factor: number) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      const r = await setIngestBudget(factor);
      throttleFactor = r.throttle_factor;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function toggleQuiesce(next: boolean) {
    if (busy) return;
    busy = true;
    errorMessage = null;
    try {
      const r = await setMeshQuiesced(next);
      quiesced = r.quiesced;
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // ── Formatters ───────────────────────────────────────────────
  // Compact, glanceable counts: 999 → "999", 14_200 → "14.2k",
  // 1_000_000 → "1M", 2_500_000_000 → "2.5B". One decimal, trailing
  // ".0" dropped so round magnitudes read clean. Mirrors the
  // `formatTokens` convention already used in MeshSettings.
  function fmtCompact(n: number): string {
    if (!Number.isFinite(n)) return "0";
    const abs = Math.abs(n);
    const scaled = (v: number) => (n / v).toFixed(1).replace(/\.0$/, "");
    if (abs < 1_000) return String(Math.round(n));
    if (abs < 1_000_000) return `${scaled(1_000)}k`;
    if (abs < 1_000_000_000) return `${scaled(1_000_000)}M`;
    return `${scaled(1_000_000_000)}B`;
  }

  function fmtBytes(bytes: number): string {
    if (bytes >= 1073741824) return `${(bytes / 1073741824).toFixed(1)} GB`;
    if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${bytes} B`;
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

  function relativeTime(unixSecs: number): string {
    const now = Math.floor(Date.now() / 1000);
    const delta = now - unixSecs;
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  // Covers both the local Activity events and the peer Contribution
  // events — they share the `{ timestamp, kind: { type, … } }` shape.
  function summariseEvent(e: ActivityEventDto | LedgerEventDto): string {
    const k = (e.kind?.type as string | undefined) ?? "Event";
    const f = e.kind as Record<string, unknown>;
    switch (k) {
      // Local activity
      case "LocalInferenceServed":
        return `Answered a local request — ${f.model_id ?? ""} (${Number(f.completion_tokens ?? 0)} tokens)`;
      case "EmbeddingsServed": {
        const forWhom =
          f.served_for && (f.served_for as Record<string, unknown>).actor === "peer"
            ? "a peer"
            : "local";
        return `Embedded ${Number(f.n_texts ?? 0)} texts for ${forWhom}`;
      }
      case "LocalKnowledgeServed":
        return `Answered a local knowledge query — ${f.corpus_id ?? ""} (${Number(f.chunks_returned ?? 0)} chunks)`;
      case "ChunksIngested":
        return `Ingested ${fmtCompact(Number(f.chunks ?? 0))} chunks into ${f.corpus_id ?? ""}`;
      case "CorpusEnriched":
        return `Enriched ${f.corpus_id ?? ""}`;
      case "NewsworthyFetched":
        return `Fetched ${Number(f.articles ?? 0)} newsworthy articles`;
      // Peer contribution
      case "InferenceServed":
        return `Served inference to a peer — ${f.model_id ?? ""} (${Number(f.tokens_generated ?? 0)} tokens)`;
      case "KnowledgeQueryServed":
        return `Served a knowledge query to a peer — ${f.corpus_id ?? ""} (${Number(f.chunks_returned ?? 0)} chunks)`;
      case "ShardTransferred":
        return `Shared a shard — ${fmtBytes(Number(f.bytes ?? 0))}`;
      case "StorageSnapshot":
        return `Hosting ${Array.isArray(f.corpora) ? (f.corpora as unknown[]).length : 0} corpora`;
      default:
        return k;
    }
  }

  onMount(() => {
    void corpusProgressStore.init();
    void refresh();
    pollHandle = setInterval(refresh, 5000);
  });

  onDestroy(() => {
    if (pollHandle !== null) clearInterval(pollHandle);
  });

  let currentPreset: CeilingPreset = $derived(
    status === null ? 0 : ceilingPreset(status),
  );
  let paused: boolean = $derived(
    status !== null && status.pause_remaining_secs !== null,
  );
  let yielding: boolean = $derived(
    status !== null && status.yielding_secs_remaining !== null,
  );

  // Headline totals, combining the chat slice (your own conversations,
  // read from message provenance) with the daemon ledger (serving +
  // background work). Both are "all on this machine."
  let tokensGenerated = $derived(
    (chat?.tokens_generated ?? 0) + (activity?.local_tokens_generated ?? 0),
  );
  let embeddingsProduced = $derived(
    (activity?.embeddings.local_units ?? 0) + (activity?.embeddings.peer_units ?? 0),
  );
  let activeIngests = $derived(corpusProgressStore.active);
  let hasMeshActivity = $derived(
    activity !== null &&
      (activity.peer_inference_served_requests > 0 ||
        activity.peer_knowledge_queries_served > 0 ||
        activity.embeddings.peer_requests > 0 ||
        activity.peer_bytes_served > 0 ||
        activity.peer_bytes_received > 0),
  );
</script>

<section class="sharing">
  <!-- ── Totals ──────────────────────────────────────────── -->
  <h3 class="h3">All on this machine</h3>
  <p class="hint">
    What Sovereign has done locally over the last {WINDOW_DAYS} days — your
    chats, the knowledge it served, and the corpora it built. None of it left
    your computer.
  </p>
  <div class="totals-grid">
    <div class="stat">
      <span class="stat-num">{fmtCompact(tokensGenerated)}</span>
      <span class="stat-label">tokens generated</span>
    </div>
    <div class="stat">
      <span class="stat-num">{fmtCompact(chat?.turns ?? 0)}</span>
      <span class="stat-label">questions answered</span>
    </div>
    <div class="stat">
      <span class="stat-num">{fmtCompact(chat?.chunks_retrieved ?? 0)}</span>
      <span class="stat-label">chunks retrieved</span>
    </div>
    <div class="stat">
      <span class="stat-num">{fmtCompact(activity?.total_chunks_ingested ?? 0)}</span>
      <span class="stat-label">chunks ingested</span>
    </div>
    <div class="stat">
      <span class="stat-num">{fmtCompact(embeddingsProduced)}</span>
      <span class="stat-label">embeddings produced</span>
    </div>
  </div>

  {#if activity && activity.corpora.length > 0}
    <ul class="corpus-list">
      {#each activity.corpora as c (c.corpus_id)}
        <li class="corpus-row">
          <span class="corpus-name">{c.corpus_id}</span>
          <span class="corpus-detail">
            {fmtCompact(c.chunks_ingested)} chunks ingested{#if c.enrich_runs > 0} · enriched{/if}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  {#if hasMeshActivity && activity}
    <h3 class="h3">Given to the mesh</h3>
    <p class="hint">Work this machine did for peers over the same window.</p>
    <div class="totals-grid">
      <div class="stat">
        <span class="stat-num">{fmtCompact(activity.peer_inference_served_requests)}</span>
        <span class="stat-label">inferences served</span>
      </div>
      <div class="stat">
        <span class="stat-num">{fmtCompact(activity.embeddings.peer_units)}</span>
        <span class="stat-label">texts embedded for peers</span>
      </div>
      <div class="stat">
        <span class="stat-num">{fmtCompact(activity.peer_knowledge_queries_served)}</span>
        <span class="stat-label">knowledge queries served</span>
      </div>
      <div class="stat">
        <span class="stat-num">{fmtBytes(activity.peer_bytes_served)}</span>
        <span class="stat-label">shared to peers</span>
      </div>
    </div>
  {/if}

  <!-- ── Now ─────────────────────────────────────────────── -->
  <h3 class="h3">Happening now</h3>
  {#if activeIngests.length === 0 && (!news?.last_tick) && (status?.in_flight ?? 0) === 0}
    <p class="hint">The daemon is idle. Nothing running right now.</p>
  {:else}
    <ul class="now-list">
      {#each activeIngests as p (p.corpus_id)}
        <li class="now-item">
          <span class="now-dot now-dot--active"></span>
          <span class="now-text">
            Building <strong>{p.corpus_id}</strong> — {p.phase}
            {#if p.percent > 0}({Math.round(p.percent)}%){/if}
          </span>
        </li>
      {/each}
      {#if news?.last_tick}
        <li class="now-item">
          <span class="now-dot"></span>
          <span class="now-text">
            Newsworthy: last fetched {relativeTime(news.last_tick.observed_at)},
            {fmtCompact(news.last_tick.tracked_total)} articles tracked
          </span>
        </li>
      {/if}
      {#if (status?.in_flight ?? 0) > 0}
        <li class="now-item">
          <span class="now-dot now-dot--active"></span>
          <span class="now-text">
            Serving <strong>{status?.in_flight}</strong>
            peer {status?.in_flight === 1 ? "request" : "requests"} right now
          </span>
        </li>
      {/if}
    </ul>
  {/if}

  <!-- ── Recent feed ─────────────────────────────────────── -->
  <h3 class="h3">Recent activity</h3>
  {#if feed.length === 0}
    <p class="hint">Nothing yet. Ask a question or import some knowledge.</p>
  {:else}
    <ul class="feed">
      {#each feed as e, i (i)}
        <li class="feed-item">
          <span class="feed-text">{summariseEvent(e)}</span>
          <span class="feed-time">{relativeTime(e.timestamp)}</span>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Controls: the reins ─────────────────────────────── -->
  <h2 class="h2">The reins</h2>
  <p class="hint">Decide how hard the daemon works. Changes take effect immediately — no restart.</p>

  <h3 class="h3">Background work</h3>
  <p class="hint">
    Throttle ingestion and enrichment so the machine stays responsive while
    big corpora build in the background.
  </p>
  <div class="presets">
    {#each THROTTLE_PRESETS as t (t.value)}
      <button
        class="preset"
        class:active={Math.abs(throttleFactor - t.value) < 0.01}
        onclick={() => chooseThrottle(t.value)}
        disabled={busy}
        title={t.hint}
      >
        {t.label}
      </button>
    {/each}
  </div>

  <h3 class="h3">Mesh participation</h3>
  <label class="toggle-row">
    <input
      type="checkbox"
      checked={quiesced}
      onchange={(e) => toggleQuiesce((e.target as HTMLInputElement).checked)}
      disabled={busy}
    />
    <span class="toggle-body">
      <span class="toggle-label">Stop participating in shared work</span>
      <span class="toggle-sub">
        Stops handing work to peers and accepting theirs. Anything already
        running keeps going. Untick to rejoin.
      </span>
    </span>
  </label>

  <h3 class="h3">How much to share</h3>
  <p class="hint">
    When your machine has spare cycles, peers can use them. Pick a ceiling that
    feels right.
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

  {#if paused && status}
    <p class="state state-paused">
      Sharing paused — resumes in {formatPauseRemaining(status.pause_remaining_secs!)}
    </p>
    <div class="row">
      <button class="action" onclick={resume} disabled={busy}>Resume now</button>
    </div>
  {:else}
    <div class="row">
      <button class="action" onclick={() => pause(15 * 60)} disabled={busy}>
        Pause 15 min
      </button>
      <button class="action" onclick={() => pause(60 * 60)} disabled={busy}>
        Pause 1 hour
      </button>
      <button class="action" onclick={() => pause(0)} disabled={busy}>
        Pause until I resume
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
</section>

<style>
  /* Lavender Court substrate — matches every other Settings section. */
  .sharing {
    font-family: var(--font-sans);
    color: var(--text-secondary);
    -webkit-font-smoothing: antialiased;
  }

  .h2 {
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 36px 0 4px;
    padding-top: 24px;
    border-top: 1px solid var(--border);
    letter-spacing: -0.01em;
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

  /* ── Totals grid ── */
  .totals-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-bottom: 16px;
  }

  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 120px;
    padding: 12px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
  }

  .stat-num {
    font-size: 1.3rem;
    font-weight: 600;
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.01em;
  }

  .stat-label {
    font-size: 0.78rem;
    color: var(--text-muted);
  }

  /* ── Per-corpus list ── */
  .corpus-list {
    list-style: none;
    padding: 0;
    margin: 0 0 14px;
  }

  .corpus-row {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 0;
    font-size: 0.85rem;
  }

  .corpus-name {
    color: var(--text-secondary);
    font-weight: 500;
  }

  .corpus-detail {
    color: var(--text-muted);
  }

  /* ── Now list ── */
  .now-list {
    list-style: none;
    padding: 0;
    margin: 0 0 14px;
  }

  .now-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 0;
    font-size: 0.88rem;
    color: var(--text-secondary);
  }

  .now-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }

  .now-dot--active {
    background: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-dim);
  }

  .now-text strong {
    color: var(--text-primary);
  }

  /* ── Presets / actions ── */
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

  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  /* ── Toggle row ── */
  .toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    margin-bottom: 8px;
    cursor: pointer;
  }

  .toggle-row input {
    margin-top: 3px;
  }

  .toggle-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .toggle-label {
    font-size: 0.88rem;
    color: var(--text-primary);
    font-weight: 500;
  }

  .toggle-sub {
    font-size: 0.82rem;
    color: var(--text-muted);
    max-width: 520px;
    line-height: 1.4;
  }

  /* ── Status pills ── */
  .state {
    font-size: 0.88rem;
    margin: 12px 0;
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

  /* ── Feed ── */
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
</style>

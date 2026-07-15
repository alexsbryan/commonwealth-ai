<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  CounterCard — the verification counter for grounded turns. A gated
  answer holds every token for ~a minute or two (draft → per-claim
  verify → corrective rewrite), and the old presentation of that hold
  was one static sentence plus a token count. This card reframes the
  wait as visible diligence: three stations (Gather → Draft → Check)
  hand off inside ONE fixed frame, and the Check station stamps each
  extracted claim as the gate verifies it against the user's sources —
  including the amber "couldn't confirm → revising" moment.

  Glassbox contract: every element is driven by a live narration frame
  (`retrieval_*`, `synthesis_progress`, `claim_*` — see CounterState in
  routing.machine.ts). The card never invents progress; when frames go
  quiet it simply breathes. It renders only while a grounded turn has
  counter signal, and ChatView suppresses the redundant chip stack and
  promoted narration line while it is active.

  Idiom notes: same conventions as NarrationChip — Lavender Court
  tokens, color-mix tints, the 0.2/0.7/0.2/1 curve, CSS keyframes (not
  Svelte transitions), reduced-motion guard, mono for digits, serif
  for the user's own material (passage titles, claim texts).
-->
<script lang="ts">
  import { routingStore } from "../stores/routing.svelte";

  const counter = $derived(routingStore.counter);
  const heartbeat = $derived(routingStore.synthesisProgress);

  type StationId = "gather" | "draft" | "check";
  const STATIONS: { id: StationId; glyph: string; label: string }[] = [
    { id: "gather", glyph: "⌕", label: "Gather" },
    { id: "draft", glyph: "✎", label: "Draft" },
    { id: "check", glyph: "✓", label: "Check" },
  ];

  const station: StationId = $derived.by(() => {
    if (counter?.check) return "check";
    if (heartbeat || counter?.retrieval?.complete) return "draft";
    return "gather";
  });
  const stationIdx = $derived(STATIONS.findIndex((s) => s.id === station));

  const retrieval = $derived(counter?.retrieval ?? null);
  const check = $derived(counter?.check ?? null);
  const confirmedCount = $derived(
    check ? check.claims.filter((c) => c.verdict === "supported").length : 0,
  );

  const checkHeadline = $derived.by(() => {
    if (!check) return "";
    if (check.complete) {
      const { confirmed, flagged } = check.complete;
      return flagged > 0
        ? `${confirmed} confirmed · ${flagged} revised from the sources`
        : `${confirmed} claim${confirmed === 1 ? "" : "s"} confirmed`;
    }
    if (check.revising !== null) {
      return `Couldn't confirm ${check.revising} — revising from the sources…`;
    }
    if (check.claims.length > 0) {
      return check.recheck
        ? `Re-checking the revised answer — ${check.claims.length} claims`
        : `Checking ${check.claims.length} claims against your sources`;
    }
    return "Reading the draft back against your sources…";
  });

  const elapsedMs = $derived(
    Math.max(counter?.elapsedMs ?? 0, heartbeat?.elapsedMs ?? 0),
  );

  function formatElapsed(ms: number): string {
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    return `${Math.floor(s / 60)}m ${(s % 60).toString().padStart(2, "0")}s`;
  }
</script>

{#if counter || heartbeat}
  <div
    class="counter-card"
    data-testid="counter-card"
    data-station={station}
    aria-label="svrnmesh is preparing a verified answer"
  >
    <div class="rail">
      {#each STATIONS as s, i (s.id)}
        {#if i > 0}<span class="rail-link" class:walked={i <= stationIdx}
          ></span>{/if}
        <span
          class="station"
          class:active={i === stationIdx}
          class:done={i < stationIdx}
          data-testid="counter-station-{s.id}"
        >
          <span class="station-glyph" aria-hidden="true"
            >{i < stationIdx ? "✓" : s.glyph}</span
          >
          <span class="station-label">{s.label}</span>
        </span>
      {/each}
    </div>

    <div class="stage">
      {#if station === "gather"}
        {#if retrieval?.complete}
          <div class="stage-line">
            Read <span class="num">{retrieval.chunksIn}</span> passages across
            <span class="num">{retrieval.corpora.length || 1}</span>
            source{retrieval.corpora.length === 1 ? "" : "s"}
          </div>
        {:else}
          <div class="stage-line">Searching your notebooks&hellip;</div>
        {/if}
        {#if retrieval && retrieval.topTitles.length > 0}
          <ul class="titles">
            {#each retrieval.topTitles as title (title)}
              <li><span class="title-mark" aria-hidden="true">{"◈"}</span>{title}</li>
            {/each}
          </ul>
        {/if}
      {:else if station === "draft"}
        <div class="stage-line">
          {#if heartbeat}
            writing…
            {#key heartbeat.tokens}
              <span class="num token-count">{heartbeat.tokens.toLocaleString()}</span>
            {/key}
            {heartbeat.tokens === 1 ? "token" : "tokens"}
          {:else}
            Warming up the primary model&hellip;
          {/if}
        </div>
        {#if retrieval?.complete}
          <div class="stage-sub">
            drafting from {retrieval.chunksIn} passages — held for verification
          </div>
        {/if}
      {:else}
        <div class="stage-line" class:revising={check?.revising !== null}>
          {checkHeadline}
        </div>
        {#if check && check.claims.length > 0}
          <ul class="claims" data-testid="counter-claims">
            {#each check.claims as claim, i (i)}
              <li class="claim {claim.verdict}">
                <span class="claim-mark" aria-hidden="true">
                  {claim.verdict === "supported"
                    ? "✓"
                    : claim.verdict === "unsupported"
                      ? "!"
                      : "·"}
                </span>
                <span class="claim-text">{claim.text}</span>
              </li>
            {/each}
          </ul>
          {#if !check.complete}
            <div class="stage-sub">
              {confirmedCount} of {check.claims.length} confirmed
            </div>
          {/if}
        {/if}
      {/if}
    </div>

    <div class="foot">
      <span class="foot-note">Verified before it reaches you</span>
      {#if elapsedMs > 0}
        <span class="foot-elapsed">{formatElapsed(elapsedMs)}</span>
      {/if}
    </div>
  </div>
{/if}

<style>
  .counter-card {
    align-self: flex-start;
    width: min(520px, 100%);
    padding: 10px 14px 8px;
    margin-bottom: 12px;
    background: color-mix(in srgb, var(--lavender) 3%, var(--bg-secondary));
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    animation: card-arrive 280ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }

  /* ── Station rail ── */
  .rail {
    display: flex;
    align-items: center;
    margin-bottom: 8px;
  }
  .station {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 0.68rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .station-glyph {
    font-size: 0.8rem;
    line-height: 1;
    color: var(--border-bright);
  }
  .station.active {
    color: var(--text-secondary);
  }
  .station.active .station-glyph {
    color: var(--accent);
    animation: station-breathe 2.4s ease-in-out infinite;
  }
  .station.done {
    color: var(--text-muted);
  }
  .station.done .station-glyph {
    color: var(--growth);
  }
  .rail-link {
    flex: 1;
    height: 1px;
    min-width: 14px;
    margin: 0 8px;
    background: var(--border-mid);
  }
  .rail-link.walked {
    background: color-mix(in srgb, var(--accent) 45%, var(--border-mid));
  }

  /* ── Stage (station body) ── */
  .stage {
    min-height: 34px;
  }
  .stage-line {
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .stage-line.revising {
    color: var(--warning);
    animation: revise-pulse 1.8s ease-in-out infinite;
  }
  .stage-sub {
    margin-top: 3px;
    font-size: 0.72rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .num {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-weight: 600;
    color: var(--accent);
    font-variant-numeric: tabular-nums;
  }
  .token-count {
    display: inline-block;
    animation: token-pop 220ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }

  /* Passage titles — the user's own material reads in the serif. */
  .titles {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }
  .titles li {
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 0.8rem;
    color: var(--text-secondary);
    padding: 1.5px 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    animation: row-arrive 380ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  .title-mark {
    color: var(--lavender);
    font-style: normal;
    font-size: 0.7rem;
    margin-right: 6px;
  }

  /* Claim rows — stamped one by one as verdicts land. */
  .claims {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
  }
  .claim {
    display: flex;
    align-items: baseline;
    gap: 7px;
    padding: 2px 0;
    animation: row-arrive 380ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  .claim-mark {
    flex: none;
    width: 12px;
    text-align: center;
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--text-muted);
  }
  .claim.supported .claim-mark {
    color: var(--growth);
  }
  .claim.unsupported .claim-mark {
    color: var(--warning);
  }
  .claim-text {
    font-family: var(--font-serif);
    font-size: 0.8rem;
    color: var(--text-muted);
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .claim.supported .claim-text,
  .claim.unsupported .claim-text {
    color: var(--text-secondary);
  }

  /* ── Footer ── */
  .foot {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-top: 8px;
    padding-top: 6px;
    border-top: 1px solid var(--border);
  }
  .foot-note {
    font-size: 0.68rem;
    font-style: italic;
    color: var(--text-muted);
  }
  .foot-elapsed {
    margin-left: auto;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.7rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  @keyframes card-arrive {
    from {
      opacity: 0;
      transform: translateY(-2px) scale(0.98);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }
  @keyframes row-arrive {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  @keyframes station-breathe {
    0%,
    100% {
      opacity: 0.55;
    }
    50% {
      opacity: 1;
    }
  }
  @keyframes revise-pulse {
    0%,
    100% {
      opacity: 0.7;
    }
    50% {
      opacity: 1;
    }
  }
  @keyframes token-pop {
    from {
      transform: translateY(-1px) scale(1.08);
      color: color-mix(in srgb, var(--accent) 70%, var(--text-secondary));
    }
    to {
      transform: translateY(0) scale(1);
      color: var(--accent);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .counter-card,
    .titles li,
    .claim,
    .station.active .station-glyph,
    .stage-line.revising,
    .token-count {
      animation: none;
    }
  }
</style>

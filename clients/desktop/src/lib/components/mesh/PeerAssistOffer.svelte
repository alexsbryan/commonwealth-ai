<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Self-contained "get peer help" affordance for a heavy ingest. Probes which
  mesh peers can help; renders a collapsed opt-in disclosure with the peer
  picker + the ephemerality guarantees. Reports the decision up via `onChange`;
  the host owns kickoff.

  When it CAN'T help, callers on an ingest surface should pass
  `explainWhenUnavailable` so the component says why instead of vanishing.
  Silently rendering nothing was the 2026-07-27 bug: a user watching a slow
  Obsidian vault embed had no peer-assist affordance and no way to tell whether
  the feature was missing, their mesh was empty, or the corpus was ineligible.
  Three very different situations collapsed into an empty div. Surfaces that
  merely *decorate* (status chips) still default to hiding.
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { meshAssistEligiblePeers } from "../../api";
  import type { AssistEligiblePeer } from "../../types";
  import PeerAssistPicker from "./PeerAssistPicker.svelte";
  import { peerCountLabel } from "./assistFormat";

  interface Props {
    corpusId: string;
    /** Drives copy nuance / telemetry. */
    surface: "vault" | "folder" | "watched" | "recipe";
    /** Expand by default (e.g. when the host knows the corpus is heavy). */
    defaultExpanded?: boolean;
    /**
     * On ingest surfaces, render an honest one-line reason when assist isn't
     * available instead of rendering nothing. Off by default so decorative
     * surfaces stay quiet.
     */
    explainWhenUnavailable?: boolean;
    onChange: (decision: {
      enabled: boolean;
      peerNodeIds: string[];
    }) => void;
  }

  let {
    corpusId,
    surface,
    defaultExpanded = false,
    explainWhenUnavailable = false,
    onChange,
  }: Props = $props();

  let peers = $state<AssistEligiblePeer[]>([]);
  let grantable = $state(false);
  let loaded = $state(false);
  // Whether the probe itself succeeded. Without this, "the daemon is down"
  // and "your mesh has no peers" both look like `peers = []`, and we'd tell
  // the user the wrong thing about a fixable situation.
  let reachable = $state(true);
  // `defaultExpanded` seeds the disclosure once; `untrack` documents that we
  // intentionally capture only its initial value (no reactive re-sync).
  let expanded = $state(untrack(() => defaultExpanded));
  let selected = $state<string[]>([]);

  let eligible = $derived(peers.filter((p) => p.eligible));
  // Show the offer only when this corpus can actually be assisted.
  let showable = $derived(loaded && grantable && peers.length > 0);
  let hasEligible = $derived(eligible.length > 0);

  // Why the offer can't be shown, in the user's terms. Ordered most-actionable
  // first: an unreachable daemon is a different fix from an empty mesh, which
  // is a different fix from an ineligible corpus.
  let unavailableReason = $derived(
    !reachable
      ? "Mesh help unavailable — can't reach the mesh service on this machine."
      : peers.length === 0
        ? "Mesh help unavailable — no other machines have joined your mesh yet."
        : !grantable
          ? "Mesh help unavailable — this source isn't shareable with peers."
          : null,
  );

  async function refresh() {
    try {
      const resp = await meshAssistEligiblePeers(corpusId);
      peers = resp.peers;
      grantable = resp.grantable;
      reachable = true;
      // Default-select eligible+online peers the first time we see them.
      if (!loaded) {
        selected = eligible.map((p) => p.node_id);
      } else {
        // Drop any selected peer that's no longer eligible.
        const stillOk = new Set(eligible.map((p) => p.node_id));
        selected = selected.filter((id) => stillOk.has(id));
      }
      loaded = true;
      emit();
    } catch {
      // Mesh not running / daemon unreachable → local-only. Recorded as
      // UNREACHABLE rather than "no peers" so `explainWhenUnavailable`
      // surfaces can tell the user which of the two it actually is.
      peers = [];
      grantable = false;
      reachable = false;
      loaded = true;
      emit();
    }
  }

  function emit() {
    onChange({
      enabled: expanded && selected.length > 0,
      peerNodeIds: [...selected],
    });
  }

  function toggle(nodeId: string) {
    selected = selected.includes(nodeId)
      ? selected.filter((id) => id !== nodeId)
      : [...selected, nodeId];
    emit();
  }

  function selectAll() {
    selected = eligible.map((p) => p.node_id);
    emit();
  }
  function clearAll() {
    selected = [];
    emit();
  }

  function setExpanded(v: boolean) {
    expanded = v;
    emit();
  }

  $effect(() => {
    // Re-probe on corpus change + poll while mounted so a peer coming online
    // becomes selectable without reopening.
    void corpusId; // track
    refresh();
    const iv = setInterval(refresh, 5000);
    return () => clearInterval(iv);
  });
</script>

{#if showable}
  <div class="offer">
    {#if !expanded}
      <button type="button" class="offer-toggle" onclick={() => setExpanded(true)}>
        <span class="spark" aria-hidden="true">✦</span>
        {#if hasEligible}
          Speed this up with your mesh — {peerCountLabel(eligible.length)} can help
        {:else}
          Mesh help unavailable — no compatible peer online
        {/if}
      </button>
    {:else}
      <div class="offer-open">
        <PeerAssistPicker
          {peers}
          {selected}
          onToggle={toggle}
          onSelectAll={selectAll}
          onClear={clearAll}
        />

        {#if hasEligible}
          <ul class="guarantees">
            <li><b>You pick the peers.</b> Only the ones you tick help.</li>
            <li><b>One-time, revocable.</b> A single grant for this job.</li>
            <li><b>Nothing is kept.</b> Peers compute, return results, and discard the source.</li>
            <li><b>Verified locally.</b> We re-check a sample here before trusting it.</li>
          </ul>
        {:else}
          <p class="fallback">
            No mesh peer can help with this one right now — it'll index on this
            machine.
          </p>
        {/if}

        <button type="button" class="link dismiss" onclick={() => setExpanded(false)}>
          Not now
        </button>
      </div>
    {/if}
  </div>
{:else if explainWhenUnavailable && loaded && unavailableReason}
  <div class="offer unavailable">
    <span class="spark" aria-hidden="true">✦</span>
    <span>{unavailableReason}</span>
  </div>
{/if}

<style>
  .offer {
    border: 1px solid var(--border-mid);
    border-radius: 8px;
    padding: 0.5rem 0.75rem;
    margin: 0.5rem 0;
    background: var(--bg-secondary);
  }
  .unavailable {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
    opacity: 0.75;
  }
  .offer-toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    cursor: pointer;
    font-size: 0.9rem;
    width: 100%;
    text-align: left;
  }
  .spark {
    color: var(--color-accent, #6a5acd);
  }
  .offer-open {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .guarantees {
    list-style: none;
    padding: 0;
    margin: 0.25rem 0;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.82rem;
    opacity: 0.9;
  }
  .fallback {
    font-size: 0.85rem;
    opacity: 0.8;
    margin: 0.25rem 0;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    cursor: pointer;
    text-decoration: underline;
    font-size: 0.8rem;
    opacity: 0.75;
  }
  .dismiss {
    align-self: flex-start;
  }
</style>

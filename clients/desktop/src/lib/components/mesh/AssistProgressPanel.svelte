<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Glassbox progress for a running (or just-finished) peer-assisted ingest.
  Pure view over one AssistJobState from `assistProgress.svelte.ts`: overall
  bar + per-peer unit tallies + the terminal verification and "reverted to
  local-only" confirmations. The local ingest is never gated on this — revoke
  only stops the peer-assist layer.
-->
<script lang="ts">
  import type { AssistJobState } from "../../stores/assistProgress.svelte";
  import {
    assistFraction,
    peerCountLabel,
    verificationOk,
    verificationSummary,
  } from "./assistFormat";

  interface Props {
    job: AssistJobState;
    onRevoke: (corpusId: string) => void;
  }

  let { job, onRevoke }: Props = $props();

  let pct = $derived(
    Math.round(assistFraction(job.complete, job.unitsTotal) * 100),
  );
  let running = $derived(job.terminal === null);
  let peerLabel = $derived(peerCountLabel(job.perPeer.length));
</script>

<div class="assist" class:done={!running}>
  <div class="assist-head">
    <span class="title">
      {#if running}
        {peerLabel} helping · {job.complete}/{job.unitsTotal} units
      {:else if job.terminal === "revoked"}
        Stopped peer help
      {:else}
        Mesh help complete
      {/if}
    </span>
    {#if running}
      <button type="button" class="stop" onclick={() => onRevoke(job.corpus_id)}>
        Stop using peers
      </button>
    {/if}
  </div>

  {#if running}
    <div class="bar"><div class="fill" style:width={`${pct}%`}></div></div>
  {/if}

  {#if job.perPeer.length > 0}
    <ul class="peers">
      {#each job.perPeer as p (p.node_id)}
        <li class="peer">
          <span class="pname">{p.node_id.slice(0, 8)}</span>
          <span class="pstat">
            {p.completed} done{#if p.leased} · {p.leased} in flight{/if}{#if p.failed} · {p.failed} failed{/if}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  {#if running && job.phase === "Merging"}
    <p class="note">merging shards on this machine…</p>
  {/if}

  {#if job.verification}
    <p class="verify" class:bad={!verificationOk(job.verification)}>
      {verificationSummary(job.verification)}
    </p>
  {/if}

  {#if !running}
    <p class="revert">Reverted to local-only. Nothing retained by peers.</p>
  {/if}

  {#if job.lastError}
    <p class="err">Progress check failed: {job.lastError}</p>
  {/if}
</div>

<style>
  .assist {
    border: 1px solid var(--border-mid);
    border-radius: 8px;
    padding: 0.6rem 0.75rem;
    margin: 0.5rem 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    font-size: 0.88rem;
  }
  .assist.done {
    opacity: 0.9;
  }
  .assist-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .title {
    font-weight: 600;
  }
  .stop {
    background: none;
    border: 1px solid var(--border-mid);
    border-radius: 6px;
    padding: 0.15rem 0.5rem;
    font-size: 0.8rem;
    cursor: pointer;
    color: inherit;
    opacity: 0.85;
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--border-mid);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--color-accent, #6a5acd);
    transition: width 0.3s ease;
  }
  .peers {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .peer {
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
    font-size: 0.82rem;
  }
  .pname {
    font-family: var(--font-mono, monospace);
    opacity: 0.8;
  }
  .pstat {
    opacity: 0.85;
  }
  .note {
    font-size: 0.82rem;
    opacity: 0.7;
    margin: 0;
  }
  .verify {
    font-size: 0.82rem;
    margin: 0;
    color: var(--color-success, #2f855a);
  }
  .verify.bad {
    color: var(--color-error, #c53030);
  }
  .revert {
    font-size: 0.82rem;
    opacity: 0.75;
    margin: 0;
  }
  .err {
    font-size: 0.8rem;
    color: var(--color-error, #c53030);
    margin: 0;
  }
</style>

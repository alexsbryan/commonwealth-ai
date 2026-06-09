<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { approvalStore } from "../stores/approval.svelte";

  // Reads both slots and the in-flight submission state directly
  // from the singleton approvalMachine — no props, no parent
  // coordination. Submitting a decision dispatches an event;
  // approvalMachine handles the Tauri invoke and clears the slot
  // on success or failure.
  let approval = $derived(approvalStore.pendingApproval);
  let inputRequest = $derived(approvalStore.pendingInput);

  // The machine's submitting state is region-specific; surface both
  // so each card can independently disable its buttons.
  let approvalSubmitting = $derived(
    approvalStore.snapshot.matches({ approval: "submitting" }),
  );
  let inputSubmitting = $derived(
    approvalStore.snapshot.matches({ input: "submitting" }),
  );

  let inputValue = $state("");

  function handleApproval(approved: boolean) {
    if (!approval || approvalSubmitting) return;
    approvalStore.send({
      type: "APPROVAL_SUBMIT",
      key: approval.key,
      approved,
    });
  }

  function handleInput() {
    if (!inputRequest || inputSubmitting || !inputValue.trim()) return;
    approvalStore.send({
      type: "INPUT_SUBMIT",
      key: inputRequest.key,
      response: inputValue.trim(),
    });
    inputValue = "";
  }
</script>

{#if approval}
  <div class="card approval-card">
    <div class="card-header">Approval Required</div>
    <div class="card-body">
      <div class="detail">
        <span class="label">Tool:</span>
        <span class="value">{approval.tool_id}</span>
      </div>
      <div class="detail">
        <span class="label">Action:</span>
        <span class="value">{approval.description}</span>
      </div>
      {#if approval.params && typeof approval.params === "object"}
        <pre class="params">{JSON.stringify(approval.params, null, 2)}</pre>
      {/if}
    </div>
    <div class="card-actions">
      <button
        class="btn deny"
        onclick={() => handleApproval(false)}
        disabled={approvalSubmitting}
      >
        Deny
      </button>
      <button
        class="btn approve"
        onclick={() => handleApproval(true)}
        disabled={approvalSubmitting}
      >
        Allow
      </button>
    </div>
  </div>
{/if}

{#if inputRequest}
  <div class="card input-card">
    <div class="card-header">Input Needed</div>
    <div class="card-body">
      <p class="question">{inputRequest.question}</p>
      <input
        type="text"
        bind:value={inputValue}
        placeholder="Type your response..."
        onkeydown={(e) => e.key === "Enter" && handleInput()}
        disabled={inputSubmitting}
      />
    </div>
    <div class="card-actions">
      <button
        class="btn approve"
        onclick={handleInput}
        disabled={inputSubmitting || !inputValue.trim()}
      >
        Submit
      </button>
    </div>
  </div>
{/if}

<style>
  .card {
    background: var(--bg-secondary);
    border: 1px solid var(--warning);
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    overflow: hidden;
    /* Parent `.messages` is a flex column; combined with `overflow:
     * hidden` above (needed for rounded-corner clipping), flex's
     * `min-height: auto` no longer expands to content. Without
     * `flex-shrink: 0` the card collapses to zero height whenever
     * messages fill the viewport. See the matching note in
     * InformationRequestCard.svelte. */
    flex-shrink: 0;
  }

  .card-header {
    background: rgba(255, 152, 0, 0.1);
    padding: 8px 16px;
    font-weight: 600;
    font-size: 0.85rem;
    color: var(--warning);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .card-body {
    padding: 12px 16px;
  }

  .detail {
    display: flex;
    gap: 8px;
    margin-bottom: 6px;
    font-size: 0.9rem;
  }

  .label {
    color: var(--text-muted);
    min-width: 50px;
  }

  .value {
    color: var(--text-primary);
  }

  .params {
    background: var(--bg-primary);
    padding: 8px 12px;
    border-radius: var(--radius);
    font-size: 0.8rem;
    color: var(--text-secondary);
    overflow-x: auto;
    margin-top: 8px;
    max-height: 120px;
    overflow-y: auto;
  }

  .question {
    margin-bottom: 8px;
    color: var(--text-primary);
  }

  input {
    width: 100%;
    padding: 8px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
  }

  input:focus {
    border-color: var(--accent);
  }

  .card-actions {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    justify-content: flex-end;
    border-top: 1px solid var(--border);
  }

  .btn {
    padding: 6px 20px;
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 0.9rem;
    transition: background 0.2s;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .approve {
    background: var(--success);
    color: var(--text-on-accent);
  }

  .approve:hover:not(:disabled) {
    background: color-mix(in srgb, var(--success) 85%, white);
  }

  .deny {
    background: var(--error);
    color: var(--text-on-accent);
  }

  .deny:hover:not(:disabled) {
    background: color-mix(in srgb, var(--error) 85%, white);
  }
</style>

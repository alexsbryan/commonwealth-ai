<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { prepareAnswerReport } from "../api";
  import type { TurnSnapshot } from "../api";

  // "It said the wrong thing" is the complaint this product gets most
  // and the one it was worst at receiving: until now it arrived as
  // prose in a chat message, and every support conversation began by
  // asking the person to reproduce it.
  //
  // Two things this dialog is careful about:
  //
  // 1. **The note is the point.** Machine state says what happened;
  //    only the reporter can say what should have happened. The
  //    textarea is the first thing focused and the only field that
  //    isn't derived.
  // 2. **The consent question is asked here, in context.** Whether to
  //    include the text of the retrieved passages is not a setting and
  //    not a default we get to pick on someone's behalf — it is a
  //    question about their documents, asked at the moment they can
  //    see which documents those are.

  interface Props {
    turn: TurnSnapshot;
    /** Titles of the passages the answer used, echoed so the consent
     *  question is about something concrete rather than abstract. */
    sourceTitles: string[];
    onclose: () => void;
  }

  let { turn, sourceTitles, onclose }: Props = $props();

  let note = $state("");
  let includeSourceText = $state(false);
  let busy = $state(false);
  let error = $state("");
  let result: { code: string | null; path: string } | null = $state(null);

  async function submit() {
    busy = true;
    error = "";
    try {
      const info = await prepareAnswerReport(
        {
          ...turn,
          include_source_text: includeSourceText,
          // Belt and braces with the Rust renderer's own gate: if
          // consent was withheld, the passage text never leaves this
          // function, let alone this machine.
          retrieved: (turn.retrieved ?? []).map((r) => ({
            ...r,
            snippet: includeSourceText ? r.snippet : null,
          })),
        },
        note,
      );
      result = {
        code: info.reference_code ?? null,
        path: info.report_path,
      };
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  function focus(node: HTMLElement) {
    node.focus();
  }
</script>

<svelte:window on:keydown={onkeydown} />

<div class="scrim" role="presentation" onclick={onclose}></div>
<div
  class="dialog"
  role="dialog"
  aria-modal="true"
  aria-labelledby="report-answer-title"
>
  {#if result}
    <h2 id="report-answer-title">Report saved</h2>
    {#if result.code}
      <p class="lede">
        Your reference is
        <strong class="code">{result.code}</strong>. Quote it if you
        mention this to anyone — it identifies exactly this answer.
      </p>
    {/if}
    <p class="body">
      The file is on your Desktop. Open it and read it — everything in
      it is plain text — then send it to whoever set up your mesh.
      Nothing has been sent anywhere.
    </p>
    <code class="path">{result.path}</code>
    <div class="row">
      <button class="primary" onclick={onclose}>Done</button>
    </div>
  {:else}
    <h2 id="report-answer-title">Report this answer</h2>
    {#if turn.question}
      <p class="quoted">“{turn.question}”</p>
    {/if}

    <label class="field">
      <span>What was wrong with it? What should it have said?</span>
      <textarea
        bind:value={note}
        rows="4"
        use:focus
        placeholder="e.g. It said it had no sources on this, but the report is in my library — I added it last week."
      ></textarea>
    </label>

    {#if sourceTitles.length > 0}
      <label class="consent">
        <input type="checkbox" bind:checked={includeSourceText} />
        <span>
          <strong>Also include the text it read.</strong>
          Helps diagnose “it quoted the wrong thing”, but it copies
          passages out of
          {#if sourceTitles.length === 1}
            <em>{sourceTitles[0]}</em>
          {:else}
            <em>{sourceTitles[0]}</em> and {sourceTitles.length - 1} other
            {sourceTitles.length - 1 === 1 ? "document" : "documents"}
          {/if}
          into the file.
        </span>
      </label>
    {/if}

    <p class="privacy">
      This writes one file to your Desktop: this question and this
      answer, how it was produced, and a check of your setup.
      {#if includeSourceText}
        Because you ticked the box, it will also include the text of
        the passages above.
      {:else}
        It lists which documents were used, <strong>not</strong> what
        they say.
      {/if}
      Nothing from your other conversations is included, and nothing
      is sent anywhere — you read it, then you send it.
    </p>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <div class="row">
      <button class="primary" onclick={submit} disabled={busy}>
        {busy ? "Writing…" : "Create report"}
      </button>
      <button class="linkish" onclick={onclose} disabled={busy}>
        Cancel
      </button>
    </div>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    z-index: 80;
  }

  .dialog {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 81;
    width: min(520px, calc(100vw - 48px));
    max-height: calc(100vh - 96px);
    overflow-y: auto;
    padding: 20px 22px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.28);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  h2 {
    margin: 0;
    font-size: 1rem;
    font-weight: 600;
  }

  .lede,
  .body,
  .privacy,
  .error {
    margin: 0;
    font-size: 0.82rem;
    line-height: 1.5;
  }

  .privacy {
    color: var(--text-muted);
  }

  .error {
    color: var(--danger, #e5484d);
  }

  .code {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 1.05rem;
    letter-spacing: 0.08em;
  }

  .quoted {
    margin: 0;
    padding-left: 10px;
    border-left: 2px solid var(--border);
    color: var(--text-muted);
    font-size: 0.82rem;
    font-style: italic;
    /* A long question shouldn't push the note field off-screen; the
       point of echoing it is recognition, not re-reading. */
    max-height: 4.5em;
    overflow: hidden;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 0.82rem;
  }

  textarea {
    font: inherit;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary, transparent);
    color: inherit;
    resize: vertical;
  }

  .consent {
    display: flex;
    gap: 9px;
    align-items: flex-start;
    font-size: 0.8rem;
    line-height: 1.45;
    padding: 9px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .consent input {
    margin-top: 2px;
    flex: none;
  }

  .path {
    display: block;
    font-size: 0.74rem;
    padding: 7px 9px;
    background: var(--bg-primary, transparent);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow-wrap: anywhere;
  }

  .row {
    display: flex;
    gap: 10px;
    align-items: center;
    margin-top: 2px;
  }

  .primary {
    font: inherit;
    font-size: 0.82rem;
    padding: 7px 14px;
    border: none;
    border-radius: var(--radius);
    background: var(--accent, #3b7ddd);
    color: #fff;
    cursor: pointer;
  }
  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .linkish {
    font: inherit;
    font-size: 0.82rem;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    text-decoration: underline;
  }
</style>

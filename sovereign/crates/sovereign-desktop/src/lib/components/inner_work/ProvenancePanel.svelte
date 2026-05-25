<script lang="ts">
  /// Glassbox panel for the inner-work surface. Toggled by Cmd+?, this
  /// renders the most recent witness-turn provenance: the assembled
  /// system prompt, the recalled memories, the conversation history
  /// slice (today: empty — the streaming witness path doesn't send
  /// prior turns to the model), the model id + token budget, Pass A
  /// timing, and the situated-context fields the runtime spliced in.
  ///
  /// The motivation is to make "the response felt trite" debuggable
  /// without instrumenting the runtime. The user opens this on a bad
  /// reply and reads top-down what the model actually saw — empty
  /// memory recall? a system prompt that doesn't fit the entry?
  /// no contradiction surfaced when one was visible? — and the
  /// answer is in front of them in the same column they're writing
  /// in, with the same typography.
  import type { TurnProvenance, RecalledMemoryProv } from "../../api";
  import { forgetMemory, weakenMemory } from "../../api";

  interface Props {
    provenance: TurnProvenance | null;
    /// True while we're waiting for a fetch — prevents the panel from
    /// flashing "no provenance" before the round-trip completes.
    loading: boolean;
    onClose: () => void;
    /// Re-fetch the provenance after the user invalidates a memory.
    /// The panel updates so the dropped memory disappears and the
    /// user sees the witness's recall surface change in real time.
    onRefresh?: () => void;
  }

  let { provenance, loading, onClose, onRefresh }: Props = $props();

  // Per-memory action state. `pending[id]` gates buttons during the
  // round-trip and provides a brief visual confirm before refresh.
  let pending: Record<string, "forget" | "weaken" | null> = $state({});

  async function handleForget(memoryId: string) {
    if (pending[memoryId]) return;
    pending = { ...pending, [memoryId]: "forget" };
    try {
      await forgetMemory(memoryId);
      onRefresh?.();
    } catch (e) {
      console.warn("inner-work: forget memory failed:", e);
    } finally {
      pending = { ...pending, [memoryId]: null };
    }
  }

  async function handleWeaken(memoryId: string) {
    if (pending[memoryId]) return;
    pending = { ...pending, [memoryId]: "weaken" };
    try {
      await weakenMemory(memoryId);
      onRefresh?.();
    } catch (e) {
      console.warn("inner-work: weaken memory failed:", e);
    } finally {
      pending = { ...pending, [memoryId]: null };
    }
  }

  // Each section is independently collapsible. The system prompt is
  // open by default — it's the most-asked-for piece. Other sections
  // collapse to reduce visual weight; the user expands what's
  // relevant to the bad reply they're inspecting.
  let openSections = $state({
    model: false,
    sent: true,
    history: true,
    memories: true,
    contradiction: true,
    situated: false,
    tensions: false,
    prompt: true,
  });

  function toggle(key: keyof typeof openSections) {
    openSections = { ...openSections, [key]: !openSections[key] };
  }

  function fmtRelative(epochSeconds: number): string {
    const then = new Date(epochSeconds * 1000);
    const now = new Date();
    const diffMs = now.getTime() - then.getTime();
    const diffMin = Math.floor(diffMs / 60_000);
    if (diffMin < 1) return "just now";
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return `${diffHr}h ago`;
    const diffDay = Math.floor(diffHr / 24);
    if (diffDay < 7) return `${diffDay}d ago`;
    return then.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
  }

  function memorySnippet(m: RecalledMemoryProv): string {
    const trimmed = m.content.trim();
    if (trimmed.length <= 280) return trimmed;
    return trimmed.slice(0, 280).trimEnd() + "…";
  }

  function fmtThinking(v: boolean | null): string {
    if (v === null) return "default";
    return v ? "on" : "off";
  }
</script>

<aside class="provenance" aria-label="Turn provenance">
  <header class="head">
    <span class="title">provenance</span>
    {#if provenance}
      <span class="captured">captured {fmtRelative(provenance.captured_at)}</span>
    {/if}
    <button
      type="button"
      class="close"
      onclick={onClose}
      aria-label="Close provenance"
      title="Esc"
    >×</button>
  </header>

  {#if loading}
    <p class="empty">loading…</p>
  {:else if !provenance}
    <p class="empty">
      No witness response captured yet for this entry. Summon the witness
      with <kbd>⌘</kbd>↵, then reopen this panel to see what was sent.
    </p>
  {:else}
    <!-- Model + budget — the smallest piece, but the answer to "is this
         even on the right slot." -->
    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("model")}
        aria-expanded={openSections.model}
      >
        <span class="caret" class:open={openSections.model}>▸</span>
        <span class="label">model + budget</span>
      </button>
      {#if openSections.model}
        <dl class="kv">
          <dt>register</dt><dd>{provenance.register}</dd>
          <dt>model id</dt><dd>{provenance.model_id ?? "—"}</dd>
          <dt>max tokens</dt><dd>{provenance.max_tokens ?? "—"}</dd>
          <dt>thinking mode</dt><dd>{fmtThinking(provenance.enable_thinking)}</dd>
          {#if provenance.pass_a_ms !== null}
            <dt>pass A (contradiction check)</dt>
            <dd>{provenance.pass_a_ms}ms</dd>
          {/if}
          <dt>system prompt size</dt>
          <dd>{provenance.system_prompt_chars.toLocaleString()} chars</dd>
        </dl>
      {/if}
    </section>

    <!-- What the user sent — verbatim, so we can compare to what the
         witness "heard." -->
    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("sent")}
        aria-expanded={openSections.sent}
      >
        <span class="caret" class:open={openSections.sent}>▸</span>
        <span class="label">your message ({provenance.user_message.length} chars)</span>
      </button>
      {#if openSections.sent}
        <pre class="code">{provenance.user_message}</pre>
      {/if}
    </section>

    <!-- History summary — the empty `sent_to_model` is the diagnosis. -->
    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("history")}
        aria-expanded={openSections.history}
      >
        <span class="caret" class:open={openSections.history}>▸</span>
        <span class="label">conversation history</span>
      </button>
      {#if openSections.history}
        <dl class="kv">
          <dt>messages on conversation</dt>
          <dd>
            {provenance.history_summary.total_messages}
            ({provenance.history_summary.user_count} user
            / {provenance.history_summary.assistant_count} witness)
          </dd>
          <dt>passed to model</dt>
          <dd>
            {provenance.history_summary.sent_to_model.length === 0
              ? "0 — witness saw only your latest message + system"
              : `${provenance.history_summary.sent_to_model.length} turns`}
          </dd>
        </dl>
        {#if provenance.history_summary.sent_to_model.length > 0}
          <ul class="list">
            {#each provenance.history_summary.sent_to_model as entry}
              <li>
                <span class="role">{entry.role}</span>
                <span class="preview">{entry.content_preview}</span>
                {#if entry.full_chars > entry.content_preview.length}
                  <span class="more">… {entry.full_chars - entry.content_preview.length} more chars</span>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </section>

    <!-- Recalled memories — the relational floor's pre-turn FTS5 + cosine
         pull. Empty here means the recall returned nothing relevant. -->
    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("memories")}
        aria-expanded={openSections.memories}
      >
        <span class="caret" class:open={openSections.memories}>▸</span>
        <span class="label">
          recalled memories ({provenance.recalled_memories.length})
        </span>
      </button>
      {#if openSections.memories}
        {#if provenance.recalled_memories.length === 0}
          <p class="empty inline">
            No memories surfaced. The relational register pre-turn recall
            returned nothing relevant — the witness has only your latest
            message to work with.
          </p>
        {:else}
          <ul class="list">
            {#each provenance.recalled_memories as mem, i}
              <li>
                <span class="memory-meta">
                  #{i + 1} · {fmtRelative(mem.created_at)}
                  {#if mem.kind === "summary"}
                    · <span class="kind-tag" title="Mechanical distillation of {mem.source_memory_ids?.length ?? 0} earlier entries by the compaction worker.">
                      summary of {mem.source_memory_ids?.length ?? 0}
                    </span>
                  {/if}
                </span>
                <p class="memory-body">{memorySnippet(mem)}</p>
                {#if mem.kind === "summary" && mem.source_memory_ids && mem.source_memory_ids.length > 0}
                  <details class="memory-sources">
                    <summary>folded source ids</summary>
                    <ul>
                      {#each mem.source_memory_ids as src_id}
                        <li><code>{src_id}</code></li>
                      {/each}
                    </ul>
                    <p class="hint">
                      Run <code>sovereign memory expand {mem.id}</code> to print the originals.
                    </p>
                  </details>
                {/if}
                <div class="memory-actions">
                  <button
                    type="button"
                    class="mem-btn"
                    onclick={() => handleWeaken(mem.id)}
                    disabled={!!pending[mem.id]}
                    title="Halve this memory's confidence — still recallable but at reduced weight"
                  >{pending[mem.id] === "weaken" ? "weakening…" : "weaken"}</button>
                  <button
                    type="button"
                    class="mem-btn destructive"
                    onclick={() => handleForget(mem.id)}
                    disabled={!!pending[mem.id]}
                    title="Tombstone this memory — never recalled again, but the row is preserved"
                  >{pending[mem.id] === "forget" ? "dropping…" : "drop"}</button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </section>

    {#if provenance.contradiction}
      <section>
        <button
          type="button"
          class="section-toggle"
          onclick={() => toggle("contradiction")}
          aria-expanded={openSections.contradiction}
        >
          <span class="caret" class:open={openSections.contradiction}>▸</span>
          <span class="label">contradiction surfaced (Pass A)</span>
        </button>
        {#if openSections.contradiction}
          <dl class="kv">
            <dt>prior</dt><dd>{provenance.contradiction.prior_evidence}</dd>
            <dt>now</dt><dd>{provenance.contradiction.current_claim}</dd>
          </dl>
        {/if}
      </section>
    {/if}

    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("situated")}
        aria-expanded={openSections.situated}
      >
        <span class="caret" class:open={openSections.situated}>▸</span>
        <span class="label">situated context</span>
      </button>
      {#if openSections.situated}
        <dl class="kv">
          <dt>current goal</dt>
          <dd>{provenance.current_goal ?? "(none)"}</dd>
          <dt>recent topic</dt>
          <dd>{provenance.recent_topic ?? "(none)"}</dd>
          <dt>last assistant excerpt (300c)</dt>
          <dd>{provenance.last_assistant_excerpt ?? "(none)"}</dd>
        </dl>
      {/if}
    </section>

    {#if provenance.temporal_tensions.length > 0}
      <section>
        <button
          type="button"
          class="section-toggle"
          onclick={() => toggle("tensions")}
          aria-expanded={openSections.tensions}
        >
          <span class="caret" class:open={openSections.tensions}>▸</span>
          <span class="label">
            temporal tensions ({provenance.temporal_tensions.length})
          </span>
        </button>
        {#if openSections.tensions}
          {#each provenance.temporal_tensions as t}
            <pre class="code">{t}</pre>
          {/each}
        {/if}
      </section>
    {/if}

    <section>
      <button
        type="button"
        class="section-toggle"
        onclick={() => toggle("prompt")}
        aria-expanded={openSections.prompt}
      >
        <span class="caret" class:open={openSections.prompt}>▸</span>
        <span class="label">
          full system prompt ({provenance.system_prompt_chars.toLocaleString()} chars)
        </span>
      </button>
      {#if openSections.prompt}
        <pre class="code prompt-block">{provenance.system_prompt}</pre>
      {/if}
    </section>
  {/if}
</aside>

<style>
  /* Inherits CSS variables from `.root` in InnerWorkSurface — same
     palette, same column. The panel is marginalia-on-marginalia: a
     thin vertical rule on the left, slightly muted ink, monospace
     for the verbatim blocks. */
  .provenance {
    display: block;
    margin: 2.5em 0 1.5em;
    padding: 1em 0 0.5em 1.25em;
    border: 0;
    border-left: 1.5px solid var(--inner-rule);
    color: var(--inner-ink-muted);
    font-size: 0.92em;
    animation: prov-arrive 320ms ease-out both;
  }

  @keyframes prov-arrive {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: 0.75em;
    margin-bottom: 1em;
  }

  .title {
    font-variant: small-caps;
    letter-spacing: 0.06em;
    color: var(--inner-ink);
    font-size: 0.95em;
  }

  .captured {
    color: var(--inner-ink-faint);
    font-size: 0.85em;
  }

  .close {
    margin-left: auto;
    background: transparent;
    border: 0;
    color: var(--inner-ink-faint);
    font-size: 1.1em;
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    border-radius: 3px;
    transition: color 200ms ease, opacity 200ms ease;
    opacity: 0.7;
  }

  .close:hover,
  .close:focus-visible {
    opacity: 1;
    color: var(--inner-ink-muted);
    outline: none;
  }

  .close:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  section {
    margin-bottom: 0.75em;
  }

  .section-toggle {
    display: flex;
    align-items: baseline;
    gap: 0.5em;
    width: 100%;
    background: transparent;
    border: 0;
    padding: 0.25em 0;
    color: var(--inner-ink-muted);
    font: inherit;
    text-align: left;
    cursor: pointer;
    border-radius: 3px;
  }

  .section-toggle:hover {
    color: var(--inner-ink);
  }

  .section-toggle:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .caret {
    display: inline-block;
    transition: transform 180ms ease;
    color: var(--inner-ink-faint);
    width: 0.85em;
  }

  .caret.open {
    transform: rotate(90deg);
  }

  .label {
    font-variant: small-caps;
    letter-spacing: 0.04em;
    font-size: 0.95em;
  }

  .kv {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25em 1em;
    margin: 0.5em 0 0.5em 1.35em;
  }

  .kv dt {
    color: var(--inner-ink-faint);
    font-size: 0.88em;
    font-variant: small-caps;
    letter-spacing: 0.04em;
  }

  .kv dd {
    margin: 0;
    color: var(--inner-ink);
    word-break: break-word;
  }

  .list {
    list-style: none;
    margin: 0.5em 0 0.5em 1.35em;
    padding: 0;
  }

  .list li {
    margin-bottom: 0.85em;
  }

  .role {
    font-variant: small-caps;
    letter-spacing: 0.04em;
    color: var(--inner-ink-faint);
    font-size: 0.85em;
    margin-right: 0.5em;
  }

  .preview {
    color: var(--inner-ink);
  }

  .more {
    color: var(--inner-ink-faint);
    font-size: 0.85em;
    margin-left: 0.4em;
  }

  .memory-meta {
    display: block;
    color: var(--inner-ink-faint);
    font-size: 0.82em;
    font-variant: small-caps;
    letter-spacing: 0.04em;
    margin-bottom: 0.2em;
  }

  .memory-body {
    margin: 0;
    color: var(--inner-ink);
    white-space: pre-wrap;
  }

  .memory-actions {
    display: flex;
    gap: 0.5em;
    margin-top: 0.4em;
  }

  /* Compaction-summary tag in the recalled-memory meta strip. Faint
     so it doesn't pull focus from the recall content itself; the
     `details` block below carries the load-bearing affordance. */
  .kind-tag {
    color: var(--inner-ink-faint);
    font-variant: normal;
    letter-spacing: 0;
  }

  .memory-sources {
    margin-top: 0.4em;
    font-size: 0.85em;
    color: var(--inner-ink-muted);
  }

  .memory-sources summary {
    cursor: pointer;
    color: var(--inner-ink-faint);
  }

  .memory-sources ul {
    margin: 0.3em 0 0;
    padding-left: 1.4em;
  }

  .memory-sources code {
    font-family: var(--inner-font-mono);
    font-size: 0.92em;
  }

  .memory-sources .hint {
    margin: 0.4em 0 0;
    font-style: italic;
  }

  /* Per-memory invalidation buttons. Quiet by default; only the
     "drop" action carries the destructive accent on hover. The
     weaken/drop pair is the user's reliable correction tool — when
     the witness over-extrapolates, this is how they walk it back
     without a special command or NLP detection. */
  .mem-btn {
    background: transparent;
    border: 1px solid var(--inner-rule);
    border-radius: 3px;
    padding: 0.15em 0.55em;
    font: inherit;
    font-size: 0.8em;
    color: var(--inner-ink-faint);
    cursor: pointer;
    transition: color 180ms ease, border-color 180ms ease, background 180ms ease;
  }

  .mem-btn:hover:not(:disabled),
  .mem-btn:focus-visible:not(:disabled) {
    color: var(--inner-ink-muted);
    border-color: var(--inner-rule);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.025) c h);
    outline: none;
  }

  .mem-btn:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .mem-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .mem-btn.destructive:hover:not(:disabled),
  .mem-btn.destructive:focus-visible:not(:disabled) {
    color: oklch(55% 0.12 25);
    border-color: oklch(55% 0.12 25 / 0.5);
  }

  .code {
    /* Verbatim blocks (system prompt, user message). Monospace anchors
       the eye; the slightly tighter line-height communicates "this is
       data, not prose." */
    margin: 0.5em 0 0.5em 1.35em;
    padding: 0.75em 1em;
    background: oklch(from var(--inner-bg-cool) calc(l - 0.025) c h);
    border-radius: 4px;
    font-family: var(--inner-font-mono);
    font-size: 0.82em;
    line-height: 1.55;
    color: var(--inner-ink);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 320px;
    overflow-y: auto;
  }

  .code.prompt-block {
    /* The full system prompt is the longest block — give it a taller
       max-height before we kick the user into a scroll. */
    max-height: 480px;
  }

  @media (prefers-color-scheme: dark) {
    .code {
      background: oklch(from var(--inner-bg-warm) calc(l + 0.04) c h);
    }
  }

  .empty {
    color: var(--inner-ink-faint);
    font-style: italic;
    margin: 0.5em 0;
  }

  .empty.inline {
    margin-left: 1.35em;
  }

  kbd {
    display: inline-block;
    padding: 0 4px;
    margin: 0 1px;
    font-family: var(--inner-font-mono);
    font-size: 0.85em;
    color: var(--inner-ink-muted);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.03) c h);
    border-radius: 3px;
  }
</style>

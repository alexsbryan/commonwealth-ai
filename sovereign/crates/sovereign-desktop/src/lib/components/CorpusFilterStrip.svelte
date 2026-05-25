<script lang="ts">
  // CorpusFilterStrip — user-controlled per-conversation allow-list of
  // installed parent corpora. Each chip is a toggle: clicking flips
  // the corpus's enabled state, persists via Tauri, and narrows
  // retrieval for every subsequent turn in this conversation. Layer
  // corpora (parent_corpus_id != null) are hidden — they follow their
  // parent at retrieval time.
  //
  // State model:
  // - `selected = null` (the "unset" state) means "all installed
  //   corpora" — bit-identical to pre-feature behavior. Stored on
  //   the Conversation row as NULL.
  // - `selected = Set<string>` is an explicit subset. The set holds
  //   parent corpus_ids only.
  //
  // The strip persists changes per-toggle via setConversationEnabledCorpora;
  // the row's enabled_corpora hydrates the strip's selection on mount
  // and whenever the conversation switches.

  import { onMount } from "svelte";
  import {
    listCorpora,
    setConversationEnabledCorpora,
  } from "../api";
  import type { CorpusEntry } from "../types";

  interface Props {
    conversationId: string | null;
    /** Initial selection from the Conversation row. `null` = "all
     *  installed" (the default). Reactive — when the parent re-passes
     *  a different value (conversation switch), the strip rehydrates. */
    initialEnabled: string[] | null | undefined;
    /** Returns the active conversation id, creating one if the user
     *  hasn't sent a first message yet. Lets empty-state chip clicks
     *  persist their toggle by minting a row on demand — without this
     *  the splash-screen chips would visually flip but the change
     *  would die when the next conversation switch hydrated. */
    ensureConversation: () => Promise<string>;
    /** Fires after every successful toggle with the new allow-list
     *  (or `null` if every corpus is selected — we normalize "all
     *  selected" back to null to keep the row clean and forward-
     *  compatible with newly-installed corpora). */
    onChange?: (enabled: string[] | null) => void;
  }

  let { conversationId, initialEnabled, ensureConversation, onChange }: Props =
    $props();

  let corpora: CorpusEntry[] = $state([]);
  // selected = null sentinel means "all installed corpora". Once the
  // user clicks any chip we materialize the full set, drop the clicked
  // corpus, and store the resulting subset. This keeps the
  // newly-installed-corpus-mid-conversation case sane: a new corpus
  // joins the "all installed" set automatically.
  let selected: Set<string> | null = $state(null);
  let hydratedConvId: string | null = $state(null);
  // Suppress the hydration effect while an in-flight toggle is
  // creating a conversation. Without this, the conversationId prop
  // transition (null → freshly-minted id) re-fires the effect mid-
  // toggle and clobbers the user's optimistic selection with the
  // still-null `initialEnabled` value from a parent that hasn't yet
  // observed the new row.
  let toggleInFlight: boolean = $state(false);

  // Parent corpora only — layers follow their parent at retrieval time
  // (see apply_corpus_allow_list in retrieval.rs). Mirrors
  // KnowledgeStatus.svelte's isPartition filter so internal
  // collaborative-ingest partitions never render here.
  let isPartition = (id: string): boolean =>
    /^.+-partition-(?:node-[0-9a-f]+|self)$/.test(id);
  let parentCorpora = $derived(
    corpora.filter(
      (c) =>
        c.status === "installed" && !c.parent_corpus_id && !isPartition(c.id),
    ),
  );

  // Hydrate the selection from the conversation row whenever the
  // active conversation changes. The strip is mounted once per chat
  // session and stays alive across convo switches; without this
  // effect a user toggling Wikipedia off in convo A and then jumping
  // to convo B would see B's chips reflect A's state.
  $effect(() => {
    if (toggleInFlight) return;
    if (conversationId !== hydratedConvId) {
      hydratedConvId = conversationId;
      if (initialEnabled && initialEnabled.length > 0) {
        selected = new Set(initialEnabled);
      } else {
        selected = null;
      }
    }
  });

  onMount(async () => {
    try {
      corpora = await listCorpora();
    } catch {
      corpora = [];
    }
  });

  function isEnabled(corpusId: string): boolean {
    if (selected === null) return true;
    return selected.has(corpusId);
  }

  // Two-thirds of the toggle logic lives here so the click handler
  // stays a one-liner. Returns the new allow-list (or null if every
  // parent ends up enabled, the sentinel "no filter" state).
  function nextSelection(
    current: Set<string> | null,
    parents: string[],
    clicked: string,
  ): { set: Set<string>; payload: string[] | null } {
    // Materialize the full set on first interaction with the sentinel
    // null state. Then drop the clicked id (we're toggling OFF).
    const base = current === null ? new Set(parents) : new Set(current);
    if (base.has(clicked)) {
      base.delete(clicked);
    } else {
      base.add(clicked);
    }
    // Normalize: if every installed parent is back in the set, store
    // null on the row. This keeps newly-installed corpora opt-in by
    // default for an "untouched" conversation, and only persists an
    // explicit subset once the user has actually opted out of one.
    const allEnabled = parents.every((p) => base.has(p));
    return { set: base, payload: allEnabled ? null : Array.from(base) };
  }

  async function toggle(corpusId: string) {
    const parents = parentCorpora.map((c) => c.id);
    const { set, payload } = nextSelection(selected, parents, corpusId);
    // Optimistic local update — the user sees the chip flip immediately
    // even if the Tauri round-trip stalls. On error we roll back.
    const prior = selected;
    selected = set;
    toggleInFlight = true;
    try {
      // Mint a conversation row on first chip click in the empty
      // state so the toggle has somewhere to persist. The parent's
      // ensureConversation is idempotent and resolves to the existing
      // id when one already exists.
      const convId = await ensureConversation();
      // Claim the (possibly newly-minted) id BEFORE we drop the
      // hydration suppression so the effect, when it next fires,
      // observes hydratedConvId already matching and short-circuits.
      hydratedConvId = convId;
      await setConversationEnabledCorpora(convId, payload);
      onChange?.(payload);
    } catch (e) {
      console.error("setConversationEnabledCorpora failed:", e);
      selected = prior;
    } finally {
      toggleInFlight = false;
    }
  }
</script>

{#if parentCorpora.length > 0}
  <div class="corpus-filter-strip" role="group" aria-label="Sources">
    {#each parentCorpora as corpus (corpus.id)}
      {@const enabled = isEnabled(corpus.id)}
      <button
        type="button"
        class="kb-tag"
        class:disabled={!enabled}
        onclick={() => toggle(corpus.id)}
        title={enabled
          ? `Click to mute ${corpus.name} for this conversation`
          : `Click to enable ${corpus.name} for this conversation`}
      >
        <span class="kb-tag-label">{corpus.name}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .corpus-filter-strip {
    /* Wrap to as many rows as the corpora demand instead of single-
       row horizontal scroll. Truncated names ("Stanford E…") were
       worse than a second row — corpus identity is load-bearing on
       this surface. Wraps grow vertically; in practice 2 rows is
       the typical max. */
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 10px 24px;
    flex-shrink: 0;
    border-top: 1px solid var(--border-mid);
    background: var(--bg-secondary);
  }

  .kb-tag {
    /* Compact mono tag. Smaller than the 0.7rem first pass so two
       rows hold ~6-8 chips without crowding; full corpus names
       always render (no ellipsis). Enabled = gold rule; disabled
       drops to a muted neutral. */
    font-size: 0.62rem;
    padding: 4px 9px;
    border: 1px solid var(--accent);
    border-radius: 2px;
    color: var(--accent-light);
    font-family: var(--font-mono);
    font-weight: 500;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    background: var(--accent-dim);
    cursor: pointer;
    flex-shrink: 0;
    white-space: nowrap;
    transition:
      opacity 0.12s ease,
      border-color 0.12s ease,
      background 0.12s ease,
      color 0.12s ease,
      box-shadow 0.12s ease;
  }
  .kb-tag:hover:not(.disabled) {
    border-color: var(--accent-hover);
    color: var(--accent-light);
    box-shadow: 0 0 10px rgba(201, 168, 76, 0.32);
  }
  .kb-tag.disabled {
    border-color: var(--border-mid);
    color: var(--text-muted);
    background: transparent;
    opacity: 0.55;
    text-decoration: line-through;
    text-decoration-thickness: 1px;
    box-shadow: none;
  }
  .kb-tag.disabled:hover {
    border-color: var(--text-muted);
    opacity: 0.75;
  }
  .kb-tag-label {
    /* No truncation — full corpus name reads, the strip wraps a row
       to make room. Truncated names ("Stanford E…") lose the
       affordance value: the chip exists so the user can recognize
       what they're toggling. */
    display: inline-block;
    vertical-align: bottom;
  }
</style>

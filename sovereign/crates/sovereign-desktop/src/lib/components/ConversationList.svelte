<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    listConversations,
    createConversation,
    deleteConversation,
    renameConversation,
    searchMessages,
  } from "../api";
  import type { ConversationEntry, SearchResult } from "../types";
  import MeshStatusIndicator from "./MeshStatusIndicator.svelte";
  import BrandMark from "./BrandMark.svelte";

  interface Props {
    selectedConversationId: string | null;
    onSelect: (id: string | null) => void;
  }

  let {
    selectedConversationId,
    onSelect,
  }: Props = $props();

  let conversations: ConversationEntry[] = $state([]);

  // Inline-rename state: at most one item is being edited at a time.
  let editingId: string | null = $state(null);
  let editingTitle: string = $state("");

  // Context menu state — null when hidden.
  let contextMenu: { x: number; y: number; convo: ConversationEntry } | null =
    $state(null);

  let unlisten: (() => void) | undefined;

  // ── Full-text search over message bodies (FTS5, backed by the
  // existing `search_messages` command). Surfaces the conversation a
  // hit belongs to so the user can jump straight there. ──
  let searchQuery = $state("");
  let searchResults = $state<SearchResult[]>([]);
  let searching = $state(false);
  let searchDebounce: ReturnType<typeof setTimeout> | undefined;

  // conversation_id → display title, for labelling search hits without
  // a second round-trip (the sidebar list is already loaded).
  let titleById = $derived(
    new Map(conversations.map((c) => [c.id, c.title || "New conversation"])),
  );
  let inSearch = $derived(searchQuery.trim().length >= 2);

  function onSearchInput() {
    if (searchDebounce) clearTimeout(searchDebounce);
    if (searchQuery.trim().length < 2) {
      searchResults = [];
      searching = false;
      return;
    }
    searching = true;
    searchDebounce = setTimeout(runSearch, 200);
  }

  async function runSearch() {
    const q = searchQuery.trim();
    if (q.length < 2) {
      searchResults = [];
      searching = false;
      return;
    }
    try {
      searchResults = await searchMessages(q);
    } catch (e) {
      console.error("Failed to search messages:", e);
      searchResults = [];
    } finally {
      searching = false;
    }
  }

  function clearSearch() {
    searchQuery = "";
    searchResults = [];
    searching = false;
    if (searchDebounce) clearTimeout(searchDebounce);
  }

  function openResult(conversationId: string) {
    clearSearch();
    onSelect(conversationId);
  }

  /** Match-centred snippet so the user sees *why* a result matched,
   *  not just the start of the message. Falls back to a head-truncation
   *  when the term isn't found verbatim (FTS stemming can match a
   *  variant). Collapses whitespace for a tidy one-line preview. */
  function snippet(text: string, q: string): string {
    const collapsed = text.replace(/\s+/g, " ").trim();
    const idx = collapsed.toLowerCase().indexOf(q.trim().toLowerCase());
    if (idx < 0) {
      return collapsed.length > 140 ? `${collapsed.slice(0, 140)}…` : collapsed;
    }
    const start = Math.max(0, idx - 40);
    const end = Math.min(collapsed.length, idx + q.trim().length + 100);
    return (
      (start > 0 ? "…" : "") +
      collapsed.slice(start, end) +
      (end < collapsed.length ? "…" : "")
    );
  }

  // ── Two-click delete confirm for the hover ✕ (easy to mis-click).
  // First click arms (3s window), second confirms. The deliberate
  // right-click → Delete menu path stays single-action. ──
  let pendingDeleteId: string | null = $state(null);
  let pendingDeleteTimeout: ReturnType<typeof setTimeout> | undefined;

  function armDelete(id: string, event: Event) {
    event.stopPropagation();
    if (pendingDeleteId === id) {
      disarmDelete();
      void deleteById(id);
      return;
    }
    pendingDeleteId = id;
    if (pendingDeleteTimeout) clearTimeout(pendingDeleteTimeout);
    pendingDeleteTimeout = setTimeout(() => {
      pendingDeleteId = null;
    }, 3000);
  }

  function disarmDelete() {
    pendingDeleteId = null;
    if (pendingDeleteTimeout) clearTimeout(pendingDeleteTimeout);
  }

  onMount(async () => {
    await loadConversations();
    // Keep the list in sync with backend changes (new messages, rename,
    // auto-generated titles). The Tauri backend emits this event after any
    // conversation mutation.
    unlisten = await listen("conversations:changed", () => {
      loadConversations();
    });
  });

  onDestroy(() => {
    unlisten?.();
    if (searchDebounce) clearTimeout(searchDebounce);
    if (pendingDeleteTimeout) clearTimeout(pendingDeleteTimeout);
  });

  export async function loadConversations() {
    try {
      conversations = await listConversations(50, 0);
    } catch (e) {
      console.error("Failed to load conversations:", e);
    }
  }

  async function handleNew() {
    try {
      const created = await createConversation();
      conversations = [
        {
          id: created.id,
          title: null,
          created_at: created.created_at,
          updated_at: created.created_at,
        },
        ...conversations,
      ];
      onSelect(created.id);
    } catch (e) {
      console.error("Failed to create conversation:", e);
    }
  }

  function startRename(convo: ConversationEntry, event: Event) {
    event.stopPropagation();
    editingId = convo.id;
    editingTitle = convo.title ?? "";
  }

  /** Svelte action: focus the input on mount and select its content. */
  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  function cancelRename() {
    editingId = null;
    editingTitle = "";
  }

  async function commitRename(id: string) {
    const next = editingTitle.trim();
    // Stop editing regardless of outcome — prevent double-submit via blur+Enter.
    editingId = null;
    if (!next) {
      editingTitle = "";
      return;
    }
    try {
      await renameConversation(id, next);
      // Optimistic local update; the listener on `conversations:changed`
      // will reconcile with the authoritative list.
      conversations = conversations.map((c) =>
        c.id === id ? { ...c, title: next } : c,
      );
    } catch (e) {
      console.error("Failed to rename conversation:", e);
    }
    editingTitle = "";
  }

  function onRenameKeydown(event: KeyboardEvent, id: string) {
    if (event.key === "Enter") {
      event.preventDefault();
      commitRename(id);
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelRename();
    }
  }

  async function deleteById(id: string) {
    try {
      await deleteConversation(id);
      conversations = conversations.filter((c) => c.id !== id);
      if (selectedConversationId === id) {
        onSelect(null);
      }
    } catch (e) {
      console.error("Failed to delete conversation:", e);
    }
  }

  function openContextMenu(e: MouseEvent, convo: ConversationEntry) {
    e.preventDefault();
    e.stopPropagation();
    // Flip upward if close to the bottom of the viewport.
    const MENU_H = 88;
    const y =
      e.clientY + MENU_H > window.innerHeight ? e.clientY - MENU_H : e.clientY;
    contextMenu = { x: e.clientX, y, convo };
  }

  function closeContextMenu() {
    contextMenu = null;
  }

  function contextMenuRename() {
    if (!contextMenu) return;
    const convo = contextMenu.convo;
    closeContextMenu();
    editingId = convo.id;
    editingTitle = convo.title ?? "";
  }

  async function contextMenuDelete() {
    if (!contextMenu) return;
    const id = contextMenu.convo.id;
    closeContextMenu();
    await deleteById(id);
  }

  function formatTime(epoch: number): string {
    const date = new Date(epoch * 1000);
    const now = new Date();
    const diff = now.getTime() - date.getTime();
    const days = Math.floor(diff / (1000 * 60 * 60 * 24));

    if (days === 0) return "Today";
    if (days === 1) return "Yesterday";
    if (days < 7) return `${days}d ago`;
    return date.toLocaleDateString();
  }
</script>

<div class="conversation-list">
  <div class="sidebar-brand">
    <BrandMark size={22} />
    <span class="brand-name">SOVEREIGN</span>
  </div>

  <div class="list-header">
    <button class="new-btn" onclick={handleNew}>
      <svg width="11" height="11" viewBox="0 0 11 11" fill="none" aria-hidden="true">
        <path d="M5.5 1v9M1 5.5h9" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
      </svg>
      New conversation
    </button>
    <div class="search-box">
      <svg class="search-icon" width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <circle cx="5" cy="5" r="3.5" stroke="currentColor" stroke-width="1.2"/>
        <path d="M7.7 7.7L10.5 10.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
      </svg>
      <input
        class="search-input"
        type="text"
        placeholder="Search messages…"
        bind:value={searchQuery}
        oninput={onSearchInput}
        spellcheck="false"
      />
      {#if searchQuery}
        <button
          class="search-clear"
          onclick={clearSearch}
          title="Clear search"
          aria-label="Clear search"
        >
          <svg width="9" height="9" viewBox="0 0 10 10" fill="none" aria-hidden="true">
            <path d="M1 1l8 8M9 1L1 9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
          </svg>
        </button>
      {/if}
    </div>
  </div>

  <div class="list-items">
    {#if inSearch}
      {#if searching}
        <p class="empty">Searching…</p>
      {:else if searchResults.length === 0}
        <p class="empty">No matches</p>
      {:else}
        {#each searchResults as result, i (result.conversation_id + ":" + i)}
          <div
            class="search-result"
            role="button"
            tabindex="0"
            onclick={() => openResult(result.conversation_id)}
            onkeydown={(e) =>
              e.key === "Enter" && openResult(result.conversation_id)}
          >
            <span class="result-title">
              {titleById.get(result.conversation_id) ?? "Conversation"}
            </span>
            <span class="result-snippet"
              >{snippet(result.content, searchQuery)}</span
            >
          </div>
        {/each}
      {/if}
    {:else}
      {#each conversations as convo (convo.id)}
      <div
        class="convo-item"
        class:selected={selectedConversationId === convo.id}
        role="button"
        tabindex="0"
        onclick={() => onSelect(convo.id)}
        onkeydown={(e) => e.key === "Enter" && onSelect(convo.id)}
        oncontextmenu={(e) => openContextMenu(e, convo)}
      >
        <div class="convo-body">
          {#if editingId === convo.id}
            <input
              class="convo-title-input"
              bind:value={editingTitle}
              onkeydown={(e) => onRenameKeydown(e, convo.id)}
              onblur={() => commitRename(convo.id)}
              onclick={(e) => e.stopPropagation()}
              use:focusOnMount
              spellcheck="false"
              maxlength="200"
            />
          {:else}
            <span
              class="convo-title"
              role="button"
              tabindex="-1"
              ondblclick={(e) => startRename(convo, e)}
              title="Double-click or right-click to rename"
            >
              {convo.title || "New conversation"}
            </span>
          {/if}
          <span class="convo-meta">
            {formatTime(convo.updated_at)}
          </span>
        </div>
        <button
          class="delete-btn"
          class:armed={pendingDeleteId === convo.id}
          onclick={(e) => armDelete(convo.id, e)}
          title={pendingDeleteId === convo.id
            ? "Click again to confirm delete"
            : "Delete"}
        >
          {#if pendingDeleteId === convo.id}
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" aria-hidden="true">
              <path d="M2.5 6.5l2.5 2.5 4.5-5.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          {:else}
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
              <path d="M1 1l8 8M9 1L1 9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
            </svg>
          {/if}
        </button>
      </div>
    {/each}

      {#if conversations.length === 0}
        <p class="empty">No conversations yet</p>
      {/if}
    {/if}
  </div>

  <div class="sidebar-footer">
    <MeshStatusIndicator />
  </div>
</div>

{#if contextMenu}
  <div class="ctx-backdrop" role="presentation" onclick={closeContextMenu} oncontextmenu={(e) => { e.preventDefault(); closeContextMenu(); }}></div>
  <div
    class="ctx-menu"
    style="top: {contextMenu.y}px; left: {contextMenu.x}px"
    role="menu"
  >
    <button class="ctx-item" role="menuitem" onclick={contextMenuRename}>
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="M8.5 1.5a1.41 1.41 0 0 1 2 2L3.5 10.5l-3 .5.5-3 7.5-6.5z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
      </svg>
      Rename
    </button>
    <div class="ctx-divider"></div>
    <button class="ctx-item ctx-item--danger" role="menuitem" onclick={contextMenuDelete}>
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="M1 3h10M4 3V2h4v1M2 3l1 7h6l1-7" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      Delete
    </button>
  </div>
{/if}

<style>
  .conversation-list {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  /* ── Brand ── */
  .sidebar-brand {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 16px 14px 12px;
  }

  /* The mark's drop-shadow halo lives inside BrandMark.svelte; the
     sidebar context just needs to size it down from the empty-state
     hero scale, which the `size={22}` prop handles. */

  .brand-name {
    flex: 1;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.2em;
    color: var(--text-secondary);
    text-transform: uppercase;
  }

  /* ── New button ── */
  .list-header {
    padding: 0 10px 10px;
  }

  .new-btn {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 8px 12px;
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: 0.03em;
    transition: all 0.2s;
  }

  .new-btn:hover {
    background: var(--accent-dim);
    border-color: var(--accent);
    color: var(--accent);
  }

  /* ── Search ── */
  .search-box {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
    padding: 0 10px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
  }
  .search-box:focus-within {
    border-color: var(--accent);
  }
  .search-icon {
    color: var(--text-muted);
    flex-shrink: 0;
  }
  .search-input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    outline: none;
    color: var(--text-secondary);
    font-size: 0.8rem;
    font-family: inherit;
    padding: 7px 0;
  }
  .search-input::placeholder {
    color: var(--text-muted);
  }
  .search-clear {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    color: var(--text-muted);
    padding: 2px;
    border-radius: 3px;
    transition:
      color 0.15s,
      background 0.15s;
  }
  .search-clear:hover {
    color: var(--text-primary);
    background: rgba(255, 255, 255, 0.05);
  }

  /* ── Search results ── */
  .search-result {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 8px 14px;
    border-left: 2px solid transparent;
    cursor: pointer;
    transition:
      background 0.15s,
      border-color 0.15s;
  }
  .search-result:hover {
    background: var(--bg-surface);
    border-left-color: var(--border-bright);
  }
  .result-title {
    font-size: 0.78rem;
    font-weight: 500;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .result-snippet {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.45;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  /* ── Conversation list ── */
  .list-items {
    flex: 1;
    overflow-y: auto;
    padding: 2px 0;
  }

  .convo-item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 9px 14px;
    border-left: 2px solid transparent;
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }

  .convo-item:hover {
    background: var(--bg-surface);
    border-left-color: var(--border-bright);
  }

  .convo-item.selected {
    background: var(--bg-surface);
    border-left-color: var(--accent);
  }

  .convo-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .convo-title {
    display: block;
    font-size: 0.84rem;
    font-weight: 500;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color 0.15s;
  }

  .convo-item.selected .convo-title {
    color: var(--accent);
  }

  .convo-title-input {
    display: block;
    width: 100%;
    font-size: 0.84rem;
    font-weight: 500;
    color: var(--text-primary, var(--text-secondary));
    background: var(--bg-input, var(--bg));
    border: 1px solid var(--accent);
    border-radius: 3px;
    padding: 1px 4px;
    margin: -2px -5px -2px -5px;
    outline: none;
    font-family: inherit;
  }

  .convo-meta {
    display: block;
    font-size: 0.67rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
  }

  .delete-btn {
    opacity: 0;
    flex-shrink: 0;
    color: var(--text-muted);
    padding: 3px;
    border-radius: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: opacity 0.15s, color 0.15s, background 0.15s;
  }

  .convo-item:hover .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    color: var(--error);
    background: rgba(212, 72, 72, 0.1);
  }

  /* Armed (first click) — stays visible regardless of hover so the
     confirm affordance doesn't vanish when the cursor drifts, and
     reads red to signal the next click is destructive. */
  .delete-btn.armed {
    opacity: 1;
    color: var(--error);
    background: rgba(212, 72, 72, 0.12);
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2.5rem 1rem;
    font-size: 0.82rem;
    letter-spacing: 0.03em;
  }

  /* ── Footer ── */
  .sidebar-footer {
    padding: 10px 10px 12px;
    border-top: 1px solid var(--border);
  }

  /* ── Context menu ── */
  .ctx-backdrop {
    position: fixed;
    inset: 0;
    z-index: 200;
  }

  .ctx-menu {
    position: fixed;
    z-index: 201;
    min-width: 148px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
    overflow: hidden;
    animation: ctx-appear 0.08s ease;
  }

  @keyframes ctx-appear {
    from { opacity: 0; transform: scale(0.96) translateY(-2px); }
    to   { opacity: 1; transform: scale(1)   translateY(0); }
  }

  .ctx-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 12px;
    font-size: 0.82rem;
    font-family: inherit;
    color: var(--text-secondary);
    background: none;
    border: none;
    text-align: left;
    cursor: pointer;
    transition: background 0.12s, color 0.12s;
  }

  .ctx-item:hover {
    background: rgba(255, 255, 255, 0.05);
    color: var(--text-primary);
  }

  .ctx-item--danger { color: var(--text-muted); }
  .ctx-item--danger:hover {
    color: var(--error);
    background: rgba(212, 72, 72, 0.1);
  }

  .ctx-divider {
    height: 1px;
    background: var(--border);
    margin: 2px 0;
  }
</style>

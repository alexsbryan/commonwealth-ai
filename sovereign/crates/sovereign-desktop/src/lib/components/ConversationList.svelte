<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    listConversations,
    createConversation,
    deleteConversation,
    renameConversation,
  } from "../api";
  import type { ConversationEntry } from "../types";
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

  onMount(async () => {
    await loadConversations();
    // Keep the list in sync with backend changes (new messages, rename,
    // auto-generated titles). The Tauri backend emits this event after any
    // conversation mutation.
    unlisten = await listen("conversations:changed", () => {
      loadConversations();
    });
  });

  onDestroy(() => unlisten?.());

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

  async function handleDelete(id: string, event: Event) {
    event.stopPropagation();
    await deleteById(id);
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
  </div>

  <div class="list-items">
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
          onclick={(e) => handleDelete(convo.id, e)}
          title="Delete"
        >
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
            <path d="M1 1l8 8M9 1L1 9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
    {/each}

    {#if conversations.length === 0}
      <p class="empty">No conversations yet</p>
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

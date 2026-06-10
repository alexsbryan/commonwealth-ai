// SPDX-License-Identifier: AGPL-3.0-or-later
// Outer-Work scope store.
//
// One pending corpus allow-list at a time, set when a mesh app's host
// event (`meshapp-open-outer-work` — today: the Wrapped Door card) asks
// for a fresh conversation whose retrieval is scoped to one corpus.
// Consumed by ChatView when the chat pane is empty, mirroring the
// `chatSeedStore` handoff: App.svelte writes + navigates, ChatView
// consumes + persists the allow-list on the freshly-minted
// conversation row.

let _pending: string[] | null = $state(null);

export const outerWorkScopeStore = {
  get pending(): string[] | null {
    return _pending;
  },

  /// Push a scope (parent corpus ids). Last-writer-wins, like the
  /// chat seed.
  set(enabledCorpora: string[]): void {
    _pending = enabledCorpora;
  },

  /// Consume and clear. ChatView calls this after persisting the
  /// allow-list so the next state change doesn't re-fire.
  consume(): string[] | null {
    const scope = _pending;
    _pending = null;
    return scope;
  },

  clear(): void {
    _pending = null;
  },
};

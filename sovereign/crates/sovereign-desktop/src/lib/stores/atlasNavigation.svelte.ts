// SPDX-License-Identifier: AGPL-3.0-or-later
// Atlas-navigation bridge store.
//
// When the user clicks "Open in atlas" from the ReadingSurface's
// AtomPanel, the chat view doesn't own the atlas surface — and
// AtlasSurface owns its own internal selection state. This store
// is the message channel between them:
//
//   1. AtomPanel calls `atlasNavigation.requestAtom(corpus, atom)`.
//   2. App.svelte observes `pendingAtom`; when set, switches the
//      view to "atlas".
//   3. AtlasSurface reads + clears the pendingAtom on mount /
//      effect, pre-selecting the requested atom.
//
// Why a dedicated store and not a callback through App.svelte: the
// reading surface mounts inside chat view, the atlas surface inside
// a sibling branch. They never see each other's props. A small
// store is the cheapest stable bridge that doesn't tangle App.svelte
// with extra navigation plumbing.

interface PendingAtom {
  corpusId: string;
  atomId: string;
}

let _pendingAtom = $state<PendingAtom | null>(null);

export const atlasNavigation = {
  get pendingAtom(): PendingAtom | null {
    return _pendingAtom;
  },

  /// Request that the desktop switch to the atlas view and open
  /// this atom's detail page. Idempotent — repeated calls before
  /// `take()` simply overwrite.
  requestAtom(corpusId: string, atomId: string): void {
    _pendingAtom = { corpusId, atomId };
  },

  /// Consume the pending request — returns it and clears the
  /// state so a remount of AtlasSurface doesn't replay the
  /// navigation. Returns `null` when nothing is queued.
  take(): PendingAtom | null {
    const v = _pendingAtom;
    _pendingAtom = null;
    return v;
  },

  /// Hard reset — used by tests and on view changes that should
  /// drop the pending state without consuming it.
  clear(): void {
    _pendingAtom = null;
  },
};

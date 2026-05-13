// Reading-navigation bridge store — mirror of `atlasNavigation`.
//
// When the user clicks "Open in reading" on an evidence row inside
// AtomDetail (which is mounted under the atlas view), the chat view
// owns ReadingSurface. App.svelte watches this store, flips back to
// chat, and feeds the chunk into readingSession.openCitation.
//
// Why a store instead of a direct call: AtomDetail can't see the
// outer view-switching machinery, and readingSession is only
// visible inside chat view's layout. The store is the cheapest
// decoupled hand-off.

interface PendingChunk {
  corpusId: string;
  chunkId: number;
  /** Breadcrumb label rendered by ReadingSurface's trail — surfaces
   *  "via Atlas inspector" or similar to the operator. */
  originLabel: string;
}

let _pendingChunk = $state<PendingChunk | null>(null);

export const readingNavigation = {
  get pendingChunk(): PendingChunk | null {
    return _pendingChunk;
  },

  /// Request that the desktop switch to chat view and open this
  /// chunk in the ReadingSurface. Idempotent — repeated calls
  /// before `take()` overwrite.
  requestChunk(corpusId: string, chunkId: number, originLabel: string): void {
    _pendingChunk = { corpusId, chunkId, originLabel };
  },

  /// Consume the request — return + clear so a remount or repeat
  /// view-switch doesn't replay the navigation.
  take(): PendingChunk | null {
    const v = _pendingChunk;
    _pendingChunk = null;
    return v;
  },

  clear(): void {
    _pendingChunk = null;
  },
};

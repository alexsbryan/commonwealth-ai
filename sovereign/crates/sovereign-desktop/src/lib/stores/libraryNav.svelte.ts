// SPDX-License-Identifier: AGPL-3.0-or-later
// Library navigation handoff.
//
// A one-shot cross-surface request consumed by LibraryView on mount —
// the same consume-and-clear shape as outerWorkScope / atlasNavigation.
// Home (and any future caller) sets a target before flipping the view to
// "library"; LibraryView `take()`s it once so navigating away and back
// doesn't replay the request.
//
//   - { notebookId } → open that notebook's detail (on its Ask tab).
//   - { openAdd: true } → open the Add sheet straight away.
//   - null → the plain shelf.

export type LibraryNavTarget =
  | { notebookId: string; openAdd?: undefined }
  | { openAdd: true; notebookId?: undefined };

let _pending: LibraryNavTarget | null = $state(null);

export const libraryNav = {
  get pending(): LibraryNavTarget | null {
    return _pending;
  },

  /// Request a target. Replaces any prior pending request (last-writer-wins).
  set(target: LibraryNavTarget): void {
    _pending = target;
  },

  /// Consume and clear — LibraryView calls this on mount.
  take(): LibraryNavTarget | null {
    const t = _pending;
    _pending = null;
    return t;
  },

  clear(): void {
    _pending = null;
  },
};

// SPDX-License-Identifier: AGPL-3.0-or-later
// Mesh-membership change bridge store.
//
// When the user joins a mesh through MeshJoinDialog (deep link or the
// paste-link input), the components that display mesh state don't see
// the join happen — the dialog is rendered by App.svelte, and a
// MeshSettings instance that's already mounted keeps its `running =
// false` snapshot from mount. Before this store existed, a user who
// accepted an invite while sitting on Settings → Mesh kept staring at
// the pre-join "Create a mesh" landing state until they navigated
// away and back — which reads as "the join silently failed" and
// invites retry loops.
//
// Flow:
//   1. App.svelte's `onJoined` calls `meshMembership.noteJoined()`.
//   2. MeshSettings watches `epoch` in an $effect and re-pulls mesh
//      state the moment it changes.
//   3. SettingsPanel consumes `takeSettingsNav()` to land the user on
//      the Mesh tab, whether the panel was already open or is about
//      to mount.
//
// Same rationale as atlasNavigation.svelte.ts: the dialog and the
// settings surface live in sibling branches of App.svelte and never
// see each other's props; a small store is the cheapest stable bridge.

let _epoch = $state(0);
let _settingsNavPending = $state(false);

export const meshMembership = {
  /// Monotonic counter, bumped on every membership change this app
  /// instance performs. Watch it in an $effect and re-fetch mesh
  /// state when it moves.
  get epoch(): number {
    return _epoch;
  },

  /// True while a "show the Mesh settings tab" request is queued.
  get settingsNavPending(): boolean {
    return _settingsNavPending;
  },

  /// Record a completed join. Bumps `epoch` and queues a settings
  /// navigation to the Mesh tab.
  noteJoined(): void {
    _epoch += 1;
    _settingsNavPending = true;
  },

  /// Consume the pending settings navigation — returns whether one
  /// was queued and clears it, so a later manual open of Settings
  /// doesn't replay the jump to the Mesh tab.
  takeSettingsNav(): boolean {
    const v = _settingsNavPending;
    _settingsNavPending = false;
    return v;
  },

  /// Hard reset — for tests.
  clear(): void {
    _epoch = 0;
    _settingsNavPending = false;
  },
};

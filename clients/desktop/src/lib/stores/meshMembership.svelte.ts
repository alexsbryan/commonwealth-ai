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
//   3. `noteJoined` also queues a Mesh-tab navigation on the shared
//      `settingsNav` store, which SettingsPanel consumes — whether the
//      panel was already open or is about to mount.
//
// Same rationale as atlasNavigation.svelte.ts: the dialog and the
// settings surface live in sibling branches of App.svelte and never
// see each other's props; a small store is the cheapest stable bridge.

import { settingsNav } from "./settingsNav.svelte";

let _epoch = $state(0);

export const meshMembership = {
  /// Monotonic counter, bumped on every membership change this app
  /// instance performs. Watch it in an $effect and re-fetch mesh
  /// state when it moves.
  get epoch(): number {
    return _epoch;
  },

  /// Record a completed join. Bumps `epoch` and queues a settings
  /// navigation to the Mesh tab.
  ///
  /// The navigation half lives in [`settingsNav`] rather than here:
  /// it stopped being a mesh concern the moment a second surface (the
  /// reconnect banner's route to the health check) needed the same
  /// thing, and two mechanisms for "open Settings there" is one too
  /// many.
  noteJoined(): void {
    _epoch += 1;
    settingsNav.request("mesh");
  },

  /// Hard reset — for tests.
  clear(): void {
    _epoch = 0;
    settingsNav.clear();
  },
};

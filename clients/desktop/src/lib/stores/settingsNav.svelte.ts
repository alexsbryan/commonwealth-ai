// SPDX-License-Identifier: AGPL-3.0-or-later
//
// "Open Settings, on that tab" — a one-slot request queue.
//
// The health check is the fix for most of the ways this product fails
// somebody, and until now the only way to reach it was to already know
// it existed and navigate to Settings → Diagnostics. Someone whose
// engine just died is the least likely person to go looking. So the
// surfaces that KNOW something is wrong — the reconnect banner today,
// error states next — need a way to say "here, this way", and they
// live in sibling branches of App.svelte that never see the settings
// panel's props.
//
// Generalised from the mesh-join case (meshMembership.noteJoined),
// which had this exact shape hardcoded to one tab. One mechanism, so a
// second caller doesn't invent a second one.
//
// Deliberately a single slot, not a queue: two pending navigations
// would mean the user lands somewhere they didn't ask for, and the
// last request is always the one they just made.

/// Tab ids as declared in SettingsPanel's `Tab` union. Kept as a
/// string type rather than imported so a store doesn't depend on a
/// component; SettingsPanel is where the list is authoritative.
export type SettingsTab =
  | "models"
  | "mesh"
  | "sharing"
  | "tools"
  | "lessons"
  | "paths"
  | "mobile"
  | "diagnostics"
  | "about";

let _pending: SettingsTab | null = $state(null);

export const settingsNav = {
  /// The queued tab, or null. Read this to decide whether to switch
  /// the app into the settings view.
  get pending(): SettingsTab | null {
    return _pending;
  },

  /// Ask for Settings to open on `tab`. Callers that also need the
  /// app to switch views must do that themselves — this store only
  /// carries the *which tab* half, because the view switch belongs to
  /// whoever owns the router.
  request(tab: SettingsTab): void {
    _pending = tab;
  },

  /// Consume the request. Returns the tab if one was queued and
  /// clears it, so a later manual open of Settings doesn't replay the
  /// jump.
  take(): SettingsTab | null {
    const v = _pending;
    _pending = null;
    return v;
  },

  /// Hard reset — for tests.
  clear(): void {
    _pending = null;
  },
};

// Runed singleton for the pending `sovereign://join/...` URL that
// should pop the MeshJoinDialog. Two sources write to it today:
//
//   1. The Tauri `deep-link-received` listener (wired in App.svelte)
//      — fires when the OS opens a `sovereign://` URL and hands it to
//      the running app. Works in release builds where the scheme is
//      registered at install.
//
//   2. The "Paste join link" input in MeshSettings — dev-mode bypass
//      for `cargo tauri dev` where the scheme isn't OS-registered.
//
// App.svelte reads `joinLinkStore.pending` and renders MeshJoinDialog
// when it's non-null. Clearing is via `clear()` — `onClose` /
// `onJoined` callbacks from the dialog call through.
let _pending: string | null = $state(null);

export const joinLinkStore = {
  get pending() {
    return _pending;
  },
  set(link: string) {
    _pending = link;
  },
  clear() {
    _pending = null;
  },
};

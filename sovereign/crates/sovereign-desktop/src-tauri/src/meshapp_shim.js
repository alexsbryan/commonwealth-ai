// MeshApp bridge shim — the host injects this into every `meshapp-*`
// webview via Tauri's `initialization_script`. It is the SINGLE SOURCE OF
// TRUTH: `commands/meshapp.rs` embeds it with `include_str!`, and the
// Playwright wiring test injects this exact file via `addInitScript`, so
// the shim→IPC path is regression-tested headlessly (the mocked-`meshApp`
// specs never exercise it — which is where the `withGlobalTauri`-off bug
// hid until it reached a real run).
//
// Defines `window.meshApp` over the always-present IPC primitive
// `__TAURI_INTERNALS__.invoke` — the app keeps `withGlobalTauri` off, so
// there is no `window.__TAURI__`. Resolved at CALL time (not captured) so
// there's no init-script ordering dependency; camelCase args map to the
// Rust snake_case params the same way `@tauri-apps/api` calls do.
(function () {
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  window.meshApp = {
    capabilities: () => invoke("meshapp_capabilities"),
    readCorpus: (corpusId, atomIds) => invoke("meshapp_read_corpus", { corpusId, atomIds }),
    parcelAnalytics: (corpusId, businessTaxTarget) =>
      invoke("meshapp_parcel_analytics", { corpusId, businessTaxTarget }),
  };
})();

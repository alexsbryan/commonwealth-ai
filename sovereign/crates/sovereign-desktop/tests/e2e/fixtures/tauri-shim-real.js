// SPDX-License-Identifier: AGPL-3.0-or-later
// Real-mode Tauri shim. Injected via Playwright's addInitScript BEFORE
// the app bundle boots — same seam as tauri-shim.js, opposite policy:
// instead of stubbing commands with synthetic handlers, every invoke is
// forwarded over HTTP to the command bridge inside a REAL running
// sovereign-desktop process (src-tauri/src/command_bridge.rs), and real
// `app_handle.emit()` events flow back in over a single SSE stream.
//
// The page Playwright drives is therefore a real frontend talking to a
// real backend (real routing, retrieval, inference, supervisor); only
// the wire (loopback HTTP instead of Tauri IPC postMessage) differs.
//
// Globals read (set by test-base-real.ts before this script):
//   __SOVEREIGN_BRIDGE_URL__   — bridge origin (default http://127.0.0.1:9745)
//   __SOVEREIGN_SPEC_NAME__    — ledger attribution for X-Sovereign-Spec
//
// Classic script (no ESM) — addInitScript constraint, same as tauri-shim.js.
(() => {
  if (window.__TAURI_INTERNALS__) return;

  const BRIDGE = window.__SOVEREIGN_BRIDGE_URL__ || "http://127.0.0.1:9745";
  // Header values must be ISO-8859-1; keep it plain ASCII or fetch throws.
  const SPEC = (window.__SOVEREIGN_SPEC_NAME__ || "").replace(/[^\x20-\x7e]/g, "?");

  // ── Callback + listener registries (same shape as tauri-shim.js) ──
  let nextCallbackId = 1;
  const callbacks = new Map(); // id → { fn, once }
  const eventListeners = new Map(); // Map<eventName, Map<eventId, callbackId>>
  let nextEventId = 1;

  // ── Captured event log for invariant assertions ───────────────
  // Every SSE row lands here in arrival (= seq) order, whether or not
  // a page listener was registered. Specs read this to assert e.g.
  // concat(message-chunk) === message-complete.full_text.
  const captured = [];

  // Deliver an event to page listeners (mirror of synthetic emit()).
  function deliver(eventName, payload) {
    const listeners = eventListeners.get(eventName);
    if (!listeners || listeners.size === 0) return 0;
    let delivered = 0;
    const snapshot = [...listeners.entries()];
    for (const [eventId, callbackId] of snapshot) {
      const cb = callbacks.get(callbackId);
      if (!cb) continue;
      cb.fn({ id: eventId, event: eventName, payload });
      if (cb.once) {
        callbacks.delete(callbackId);
        listeners.delete(eventId);
      }
      delivered += 1;
    }
    return delivered;
  }

  // ── SSE: the bridge's event stream → page listeners ───────────
  const es = new EventSource(`${BRIDGE}/events`);
  es.onmessage = (m) => {
    let row;
    try {
      row = JSON.parse(m.data);
    } catch {
      return;
    }
    if (typeof row.lagged === "number") {
      // Consumer fell behind the bridge's buffer — make it loud, a
      // gap here invalidates stream-integrity assertions.
      captured.push({ event: "__lagged__", payload: row, seq: -1 });
      console.error(`[tauri-shim-real] SSE lagged, dropped ${row.lagged} events`);
      return;
    }
    captured.push(row);
    deliver(row.event, row.payload);
  };
  es.onerror = () => {
    // EventSource auto-reconnects; surface it for triage but don't fail
    // the page — the bridge being briefly unreachable mid-teardown is
    // normal.
    console.warn("[tauri-shim-real] SSE connection error (will retry)");
  };

  async function bridgeInvoke(cmd, args) {
    const res = await fetch(`${BRIDGE}/invoke`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-sovereign-spec": SPEC,
      },
      body: JSON.stringify({ cmd, args: args ?? {} }),
    });
    const body = await res.json();
    if (body.ok) return body.result;
    // Real invoke() rejects with the command's error value — preserve
    // that exactly so api.ts error normalization behaves identically.
    throw body.error;
  }

  // Register a page listener, wire the name through the bridge, and
  // locally replay buffered sticky lifecycle events (backend-ready,
  // setup-required, …) to THIS listener — the real emission happened
  // minutes before this page existed.
  //
  // Awaited deliberately: `listen()` must not resolve until the
  // bridge-side `listen_any` subscription exists, otherwise an event
  // emitted immediately after (send → first chunk) could be missed.
  // Real Tauri's listen() is async too, so callers already cope.
  async function bridgeListen(eventName, handlerCallbackId) {
    const eventId = nextEventId++;
    if (!eventListeners.has(eventName)) eventListeners.set(eventName, new Map());
    eventListeners.get(eventName).set(eventId, handlerCallbackId);

    try {
      const res = await fetch(`${BRIDGE}/listen`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ event: eventName }),
      });
      const body = await res.json();
      if (body.replayed) {
        const cb = callbacks.get(handlerCallbackId);
        if (cb) cb.fn({ id: eventId, event: eventName, payload: body.replay });
      }
    } catch (e) {
      console.error(`[tauri-shim-real] /listen ${eventName} failed:`, e);
    }
    return eventId;
  }

  // ── __TAURI_INTERNALS__ — what the bundle reads ──────────────
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args /*, options */) => {
      // The event plugin is page-local state (listener registries) —
      // everything else, including other plugin:* commands, dispatches
      // through the real backend.
      if (cmd === "plugin:event|listen") {
        return bridgeListen(args.event, args.handler);
      }
      if (cmd === "plugin:event|unlisten") {
        eventListeners.get(args.event)?.delete(args.eventId);
        return undefined;
      }
      return bridgeInvoke(cmd, args);
    },
    transformCallback: (callback, once) => {
      const id = nextCallbackId++;
      callbacks.set(id, { fn: callback, once: !!once });
      return id;
    },
    unregisterCallback: (id) => {
      callbacks.delete(id);
    },
    convertFileSrc: (path) => path,
  };

  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event, eventId) => {
      for (const listeners of eventListeners.values()) listeners.delete(eventId);
    },
  };

  // ── __sovereign_real__ — read-only assertion surface ───────────
  window.__sovereign_real__ = {
    /** All SSE rows in arrival order: {seq, event, payload}. */
    captured,
    /** Ordered chunk texts for one assistant message id. */
    chunksFor(messageId) {
      return captured
        .filter(
          (r) => r.event === "message-chunk" && r.payload?.message_id === messageId,
        )
        .map((r) => r.payload.chunk);
    },
    /** The message-complete payload for one message id, or null. */
    completeFor(messageId) {
      const row = captured.find(
        (r) => r.event === "message-complete" && r.payload?.message_id === messageId,
      );
      return row ? row.payload : null;
    },
    /** True if the SSE consumer ever lagged (stream assertions invalid). */
    lagged() {
      return captured.some((r) => r.event === "__lagged__");
    },
  };

  // ── __sovereign_test__ — synthetic drive surface, real-mode guard ──
  // Reads stay useful; drives throw so a spec accidentally written
  // against the synthetic harness fails loudly instead of silently
  // injecting fake events into a real session.
  const notAvailable = (name) => () => {
    throw new Error(
      `__sovereign_test__.${name} is not available in real mode — ` +
        `the backend is real; drive it through the UI or the bridge`,
    );
  };
  window.__sovereign_test__ = {
    setHandler: notAvailable("setHandler"),
    emit: notAvailable("emit"),
    signalBackendReady: () => 0, // no-op: the real backend signals itself
    streamTokens: notAvailable("streamTokens"),
    completeMessage: notAvailable("completeMessage"),
    errorMessage: notAvailable("errorMessage"),
    lastStreamStart: () => null,
    lastCancel: () => null,
    lastConsent: () => null,
    reset: () => {},
  };
})();

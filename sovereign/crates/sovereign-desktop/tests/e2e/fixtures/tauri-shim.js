// Injected via Playwright's addInitScript BEFORE the app bundle boots.
// Impersonates `window.__TAURI_INTERNALS__` so @tauri-apps/api `invoke`
// and `listen` resolve in-page without a real Tauri runtime.
//
// Two surfaces:
//   • __TAURI_INTERNALS__       — the bridge the bundle reads from
//   • __sovereign_test__        — the control surface tests drive
//
// Why this lives as a vanilla .js file: Playwright's addInitScript runs
// it as a classic script in the page, so no ESM imports here.
(() => {
  if (window.__TAURI_INTERNALS__) return;

  // ── Callback registry (transformCallback ↔ id) ───────────────
  let nextCallbackId = 1;
  const callbacks = new Map(); // id → { fn, once }

  // ── Event listeners (plugin:event|listen ↔ event name) ───────
  // Map<eventName, Map<eventId, callbackId>>
  const eventListeners = new Map();
  let nextEventId = 1;

  // ── Default invoke handlers ──────────────────────────────────
  // Each entry is (args) => result | Promise<result>. Tests can
  // override per-call via __sovereign_test__.setHandler(cmd, fn).
  const defaults = {
    is_setup_complete: () => true,
    is_first_run: () => false,
    mark_first_run_complete: () => undefined,
    detect_bootstrap: () => ({
      daemon_running: false,
      client_port: 9741,
      has_config_toml: true,
    }),
    detect_hardware: () => ({
      ram_gb: 32,
      cpu_cores: 8,
      gpu: null,
    }),
    list_conversations: () => [],
    get_conversation: ({ conversationId }) => {
      throw new Error(`conversation ${conversationId} not found`);
    },
    list_corpora: () => [],
    list_document_assets: () => [],
    list_legacy_documents: () => [],
    list_skills: () => [],
    list_insights: () => [],
    get_sink_status: () => ({ running: false, pending: 0 }),
    enrich_list_corpora: () => [],
    enrich_get_starter_questions: () => [],
    enrich_get_active_job: () => null,
    mesh_get_state: () => null,
    mesh_is_running: () => false,
    // Match the MeshDiagnostics TS type — MeshDiagnosticsPanel reads
    // `discovered_peers.length`, so an unkeyed `peers` here renders
    // a TypeError that the pageerror watcher surfaces as an
    // unrelated chaos failure in any test that opens Settings.
    mesh_diagnostics: () => ({
      discovered_peers: [],
      daemon_running: false,
    }),
    mesh_relay_candidates: () => [],
    // Mesh Health (peer preferences + dimensional contributions). The
    // shim records calls via the per-command tracker below so tests
    // can assert what arguments the UI dispatched.
    mesh_get_contributions: () => [],
    mesh_set_peer_preference: (args) => {
      window.__sovereign_test__._lastSetPreference = args;
      return undefined;
    },
    mesh_clear_peer_preference: (args) => {
      window.__sovereign_test__._lastClearPreference = args;
      return true;
    },
    mesh_list_peer_preferences: () => [],
    get_config: () => ({
      embedding_model: null,
      chat_model: null,
      mesh_enabled: false,
    }),
    diagnose_corpus: () => "ok",
    create_conversation: () => ({
      id: `conv-${Math.random().toString(36).slice(2, 10)}`,
      title: "New conversation",
      created_at: Math.floor(Date.now() / 1000),
    }),
    send_message_stream: ({ conversationId }) => {
      const messageId = `asst-${Math.random().toString(36).slice(2, 10)}`;
      // Record so tests can grab the streaming id without coordination.
      window.__sovereign_test__._lastStreamStart = {
        conversationId,
        messageId,
      };
      return { message_id: messageId };
    },
    cancel_stream: ({ conversationId }) => {
      window.__sovereign_test__._lastCancel = { conversationId };
      return undefined;
    },
    search_web: ({ query, conversationId }) => ({
      message_id: `web-${Math.random().toString(36).slice(2, 10)}`,
      content: `(stubbed web result for ${query})`,
      conversation_id: conversationId,
    }),
    ask_document: ({ assetId, question }) => ({
      response: `(stubbed answer for ${question})`,
      operation: "ask",
      sources: [],
    }),
    // Local-corpus defaults. `lc_list` returns the array directly
    // (Tauri unwraps the response). `lc_incomplete_jobs` likewise
    // returns the inner Vec, not a `{ jobs: [...] }` wrapper.
    lc_list: () => [],
    lc_incomplete_jobs: () => [],
    lc_ocr_available: () => false,
    lc_validate_path: () => ({ exists: true, is_dir: true, readable: true }),
    // Watched-folder defaults — empty list, empty incomplete jobs,
    // empty details. Per-test overrides via setHandler populate
    // realistic shapes when the spec exercises this surface.
    lc_watch_list: () => ({ corpora: [] }),
    lc_watch_incomplete_jobs: () => ({ jobs: [] }),
    lc_watch_status: ({ corpusId }) => ({
      corpus_id: corpusId,
      status: { kind: "idle", last_sweep_unix: 0, live_docs: 0, tombstones: 0 },
    }),
    lc_watch_state: ({ corpusId }) => ({
      corpus_id: corpusId,
      status: { kind: "idle", last_sweep_unix: 0, live_docs: 0, tombstones: 0 },
      skipped_by_extension: {},
      failed_files: [],
      tombstones: 0,
      live_entries: 0,
    }),
    lc_watch_details: ({ corpusId }) => ({
      corpus_id: corpusId,
      display_name: "Sample folder",
      root_path: "/tmp/sample",
      status: { kind: "idle", last_sweep_unix: 0, live_docs: 0, tombstones: 0 },
      sync_mode: "continuous",
      sensitive: false,
      live_entries: 0,
      formats: {},
      skipped_by_extension: {},
      failed_files: [],
      tombstones: 0,
      enrichment: { kind: "off" },
      last_sweep_unix: 0,
      roots: [
        {
          idx: 0,
          path: "/tmp/sample",
          added_at_unix: 0,
          doc_count: 0,
          primary: true,
        },
      ],
    }),
    // Folder-ingest v1 §3.3 — enrichment lifecycle stubs. Tests
    // that exercise enable / disable / rebuild override these
    // with realistic shapes via setHandler; the defaults just
    // keep the page from erroring on missing handlers.
    lc_watch_enrich_enable: ({ corpusId }) => ({
      corpus_id: corpusId,
      job_id: `mock-job-${Math.random().toString(36).slice(2, 10)}`,
      ok: true,
    }),
    lc_watch_enrich_disable: ({ corpusId }) => ({ corpus_id: corpusId, ok: true }),
    lc_watch_enrich_rebuild: ({ corpusId }) => ({
      corpus_id: corpusId,
      job_id: `mock-job-${Math.random().toString(36).slice(2, 10)}`,
      ok: true,
    }),
    lc_watch_document: ({ corpusId, docId }) => ({
      corpus_id: corpusId,
      doc_id: docId,
      absolute_path: `/tmp/sample/${docId}`,
      size_bytes: 0,
      mtime_unix: 0,
      content_hash: "0".repeat(16),
      chunk_count: 0,
      first_chunk_preview: null,
      atoms: [],
    }),
    submit_approval: () => true,
    submit_input: () => true,
    submit_information_response: () => true,
    resume_session: () => ({
      message_id: `asst-${Math.random().toString(36).slice(2, 10)}`,
    }),
    redirect_turn: () => ({
      message_id: `asst-${Math.random().toString(36).slice(2, 10)}`,
    }),
    // Internal Tauri event-plugin commands (intercepted, never reach a backend).
    "plugin:event|listen": ({ event, handler }) => {
      const eventId = nextEventId++;
      if (!eventListeners.has(event)) eventListeners.set(event, new Map());
      eventListeners.get(event).set(eventId, handler);
      return eventId;
    },
    "plugin:event|unlisten": ({ event, eventId }) => {
      eventListeners.get(event)?.delete(eventId);
      return undefined;
    },
  };

  // Per-test overrides set via __sovereign_test__.setHandler.
  const overrides = new Map();

  function callHandler(cmd, args) {
    const handler = overrides.get(cmd) ?? defaults[cmd];
    if (!handler) {
      // Unknown command: warn loudly so a missing stub during a test
      // is visible. Resolve with undefined to keep the app alive.
      console.warn(`[tauri-shim] unstubbed invoke: ${cmd}`, args);
      return undefined;
    }
    return handler(args);
  }

  // ── __TAURI_INTERNALS__ — what the bundle reads ──────────────
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args /*, options */) => {
      // Resolve on a microtask boundary so callers always see async
      // semantics (matches real Tauri). Errors propagate as rejections.
      try {
        const result = await callHandler(cmd, args ?? {});
        return result;
      } catch (e) {
        throw e;
      }
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

  // Some Tauri internals reach for a separate event-plugin global.
  // Stub it as a no-op so any stray reference doesn't throw.
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event, eventId) => {
      for (const listeners of eventListeners.values()) listeners.delete(eventId);
    },
  };

  // ── __sovereign_test__ — the test control surface ────────────
  function emit(eventName, payload) {
    const listeners = eventListeners.get(eventName);
    if (!listeners || listeners.size === 0) return 0;
    let delivered = 0;
    // Snapshot to avoid mutation-during-iteration when a listener
    // triggers an unlisten.
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

  // Resolves on next microtask so emits sequenced after `await page.evaluate`
  // have time to land before the next assertion.
  const tick = () => new Promise((r) => setTimeout(r, 0));

  window.__sovereign_test__ = {
    /** Override (or restore) a Tauri command handler at runtime. */
    setHandler(cmd, fn) {
      if (fn === null || fn === undefined) overrides.delete(cmd);
      else overrides.set(cmd, fn);
    },
    /** Emit a Tauri event to all live listeners. Returns delivered count. */
    emit,
    /** Drive the backend-ready handshake App.svelte is gated on. */
    signalBackendReady() {
      return emit("backend-ready", {});
    },
    /** Stream a list of tokens for a given assistant message id, then
     *  optionally complete. `gapMs` controls inter-token cadence — pass
     *  0 for a burst (no waits), 16 for ~60fps cadence, etc. */
    async streamTokens(messageId, tokens, gapMs = 0) {
      for (const tok of tokens) {
        emit("message-chunk", { message_id: messageId, chunk: tok });
        if (gapMs > 0) {
          await new Promise((r) => setTimeout(r, gapMs));
        } else {
          await tick();
        }
      }
    },
    /** Emit message-complete for the given message id. */
    completeMessage(messageId, fullText, metadata) {
      return emit("message-complete", {
        message_id: messageId,
        full_text: fullText,
        metadata: metadata ?? null,
      });
    },
    /** Emit message-error to break the in-flight stream. */
    errorMessage(message) {
      return emit("message-error", { message });
    },
    /** Read-only peek at the most recent stream-start the shim recorded. */
    lastStreamStart() {
      return this._lastStreamStart ?? null;
    },
    /** Read-only peek at the most recent cancel_stream invocation. */
    lastCancel() {
      return this._lastCancel ?? null;
    },
    /** Reset the shim between tests (Playwright recreates the page,
     *  but call this if you re-use a page across cases). */
    reset() {
      overrides.clear();
      eventListeners.clear();
      callbacks.clear();
      nextCallbackId = 1;
      nextEventId = 1;
      this._lastStreamStart = null;
      this._lastCancel = null;
    },
  };
})();

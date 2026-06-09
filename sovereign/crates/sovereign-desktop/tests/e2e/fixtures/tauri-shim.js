// SPDX-License-Identifier: AGPL-3.0-or-later
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
    // Default: resolve immediately. Onboarding specs override this with
    // a handler that drives `setup-progress` events on a fake clock —
    // see specs/onboarding.spec.ts for the scripted-progress helpers.
    complete_setup_auto: () => undefined,
    start_default_corpus_install: () => undefined,
    // Contribution controls (W3 Sharing tab). Default to a fresh
    // unpaused machine — covers the section's happy-path render
    // without spec-side overrides.
    get_contribution_status: () => ({
      ceiling: 1,
      in_flight: 0,
      paused_until: null,
      pause_remaining_secs: null,
      yield_peers_to_foreground: false,
      yielding_secs_remaining: null,
    }),
    get_recent_contributions: () => [],
    set_contribution_ceiling: ({ max }) => ({
      ceiling: max === null ? Number.MAX_SAFE_INTEGER : max,
      in_flight: 0,
      paused_until: null,
      pause_remaining_secs: null,
      yield_peers_to_foreground: false,
      yielding_secs_remaining: null,
    }),
    pause_contributions: ({ durationSecs }) => ({
      ceiling: 1,
      in_flight: 0,
      paused_until: Math.floor(Date.now() / 1000) + durationSecs,
      pause_remaining_secs: durationSecs,
      yield_peers_to_foreground: false,
      yielding_secs_remaining: null,
    }),
    resume_contributions: () => ({
      ceiling: 1,
      in_flight: 0,
      paused_until: null,
      pause_remaining_secs: null,
      yield_peers_to_foreground: false,
      yielding_secs_remaining: null,
    }),
    // First-mesh-join consent. Default `null` (= not yet recorded) is
    // accurate for a fresh install; the consent gate appears.
    get_first_mesh_consent: () => null,
    record_first_mesh_consent: ({ shareGpu }) => {
      window.__sovereign_test__._lastConsent = { shareGpu };
      return {
        share_gpu: !!shareGpu,
        ceiling: 0.5,
        recorded_at_unix: Math.floor(Date.now() / 1000),
      };
    },
    rename_conversation: () => undefined,
    // Per-conversation corpus allow-list write. Records the most
    // recent payload so specs can assert what the chip toggle sent.
    // Returns void; the desktop emits `conversations:changed` after
    // success (not modeled here — tests poll the recorded payload).
    set_conversation_enabled_corpora: (args) => {
      window.__sovereign_test__._lastEnabledCorpora = args;
      return undefined;
    },
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
      // M3 — recipe-author workspace gate. Default `true` in the
      // shim so existing specs that exercise the workspace don't
      // need to flip it; specs that want the OFF state set it via
      // setHandler before navigating.
      enable_recipe_authoring: true,
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
    // ── Recipe Author Workspace (M2) ──────────────────────────
    // Default stubs let the workspace mount + render in e2e mode
    // without a real daemon. The state is held on
    // `__sovereign_test__.recipeAuthor` so specs can manipulate it
    // via `setHandler` overrides without rewriting all the defaults.
    recipe_author_list_projects: () => {
      return window.__sovereign_test__.recipeAuthor.projects.slice();
    },
    recipe_author_new_project: ({ req }) => {
      const id = `feat-${Math.random().toString(36).slice(2, 10)}`;
      const now = Math.floor(Date.now() / 1000);
      const entry = {
        feature_id: id,
        title: req.title,
        charter_excerpt: (req.charter_md ?? "").slice(0, 200),
        recipe_id: null,
        current_sample_size: null,
        last_test_status: null,
        created_at: now,
        updated_at: now,
      };
      window.__sovereign_test__.recipeAuthor.projects.unshift(entry);
      window.__sovereign_test__.recipeAuthor.dashboards[id] = {
        feature_id: id,
        title: entry.title,
        charter_md: req.charter_md ?? "",
        recipe_id: null,
        recipe_path: null,
        recipe_toml: null,
        current_sample_size: null,
        last_test_status: null,
        last_test_at: null,
        created_at: now,
        updated_at: now,
        decisions: [],
        research_findings: [],
        capability_requests: [],
        recipe_issues: [],
        deferred_questions: [],
        checkpoints: [],
        validation: { ok: false, errors: [], no_recipe: true },
      };
      return entry;
    },
    recipe_author_dashboard_state: ({ featureId }) => {
      const d = window.__sovereign_test__.recipeAuthor.dashboards[featureId];
      if (!d) throw new Error(`unknown feature_id ${featureId}`);
      return d;
    },
    recipe_author_restore_checkpoint: ({ req }) => ({
      new_checkpoint_id: `restore-${Math.random().toString(36).slice(2, 8)}`,
      source_checkpoint_id: req.checkpoint_id,
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
    /** In-memory recipe-author state. Specs that exercise the
     *  workspace can pre-seed `projects` / `dashboards` before
     *  navigating, or read `active` to assert the skill toggle ran.
     *  Defaults are an empty workspace. */
    recipeAuthor: {
      active: false,
      projects: [],
      dashboards: {},
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
    /** Read-only peek at the most recent record_first_mesh_consent call.
     *  `{ shareGpu: true | false }` once the user has chosen; null
     *  before that. */
    lastConsent() {
      return this._lastConsent ?? null;
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
      this._lastConsent = null;
    },
  };
})();

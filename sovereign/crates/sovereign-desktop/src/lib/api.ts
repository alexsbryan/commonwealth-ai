// SPDX-License-Identifier: AGPL-3.0-or-later
import { invoke } from "@tauri-apps/api/core";
import { normalizeError } from "./errors";
import type {
  WorkflowCatalogEntry,
  WorkflowRunHandle,
} from "./types";
import type {
  MessageResponse,
  ConversationEntry,
  ConversationDetail,
  CreateConversationResponse,
  SearchResult,
  SkillEntry,
  DesktopConfig,
  SetupConfig,
  SetupContextWindow,
  DiscoveredModel,
  DownloadRequest,
  CorpusEntry,
  NotebookSummary,
  CorpusProgressPayload,
  CorpusHealthDetail,
  HardwareInfo,
  PrimaryOption,
  ProfileName,
  RecommendedProfile,
  SlotConfig,
  StreamStartedResponse,
  CreateMeshResponse,
  JoinMeshResponse,
  JoinConfirmation,
  MeshStateResponse,
  RecipeValidateResult,
  RecipeTestResult,
  HarnessRunCard,
  InsightNodeDto,
  SinkStatusDto,
  DocumentAsset,
  DocumentAskResponse,
  LegacyDocumentEntry,
  BootstrapSnapshot,
  AtlasCorpusSummary,
  AtomFilter,
  AtomListPage,
  AtlasSubgraph,
  PageCursor,
  AtomDetail,
  ChunkEntityProgressRow,
  ConvCorpusSummary,
  ConvDetailView,
  ConvEntityChip,
  ConvListPage,
  EntityAggregateRow,
  GlinerModelStatus,
} from "./types";

/// Tauri `invoke` that normalises every rejection to a `DesktopError`
/// (§2D-3). Migrated commands reject with the structured wire shape;
/// unmigrated commands (bare `String`) and JS errors are coerced to
/// `internal`. Callers can therefore `catch (e) { toastError(e) }`
/// uniformly and branch on `e.code`. Migrate a command's callers to this
/// as the command's Rust side flips to `Result<_, DesktopError>`.
export async function invokeChecked<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    // Normalise to a DesktopError, then throw it as an Error instance so
    // the many existing `e instanceof Error ? e.message : String(e)` catch
    // blocks render the message (not "[object Object]"). The structured
    // `code` + `suggested_action` ride along for callers that branch via
    // `isDesktopError(e)` / `e.code`.
    const de = normalizeError(e);
    throw Object.assign(new Error(de.message), {
      code: de.code,
      suggested_action: de.suggested_action,
    });
  }
}

export async function sendMessage(
  message: string,
  conversationId: string,
  contextChunks?: FocusedPassageRef[],
  attachedFiles?: AttachedFileRef[],
): Promise<MessageResponse> {
  return invoke("send_message", {
    message,
    conversationId,
    contextChunks,
    attachedFiles,
  });
}

/** Optional focused-passage context — when present, the desktop
 *  prepends each chunk's text to the message as a labelled
 *  "▸ passage from ..." block before the runtime sees it. Used by
 *  the reading surface's "ask about this passage" handoff. */
export interface FocusedPassageRef {
  corpus_id: string;
  chunk_id: number;
}

/** A file attached for a TOOL to act on (vision, OCR, audio transcription).
 *  Its absolute path is prepended to the message as a "▸ attached file:" block
 *  so the model can pass it to an MCP tool. Distinct from a document
 *  attachment, which is ingested for RAG. */
export interface AttachedFileRef {
  path: string;
  name: string;
  /** "image" | "audio" | "other" — nudges routing toward the right tool. */
  kind: string;
}

export async function sendMessageStream(
  message: string,
  conversationId: string,
  contextChunks?: FocusedPassageRef[],
  attachedFiles?: AttachedFileRef[],
): Promise<StreamStartedResponse> {
  return invoke("send_message_stream", {
    message,
    conversationId,
    contextChunks,
    attachedFiles,
  });
}

// ─── Antifragile-routing commands ────────────────────────────

/** PR6 — cancel the current in-flight stream for a conversation.
 *  The sampler breaks the decode loop on its next check, the
 *  stream closes, and the existing `message-complete` listener
 *  transitions chat.machine back to idle automatically. */
export async function cancelStream(conversationId: string): Promise<void> {
  return invoke("cancel_stream", { conversationId });
}

/** Eagerly load the primary chat slot. Fire-and-forget — the Tauri
 *  command spawns the load and returns immediately, so callers get
 *  a `Promise<void>` that resolves the moment the request is
 *  queued, not when the model is ready.
 *
 *  Call sites: window-focus is wired in the Tauri builder (Rust
 *  side, fires automatically); ChatView calls this on mount so the
 *  slot warms while the user is reading the empty state / picking
 *  a starter question. Idempotent — a warm slot returns
 *  immediately on the backend. */
export async function warmupPrimarySlot(): Promise<void> {
  return invoke("warmup_primary_slot");
}

/** PR2c — cancel the in-flight sampler AND start a new stream
 *  against the chosen alternative intent. Returns a
 *  `StreamStartedResponse` just like `sendMessageStream`; the caller
 *  listens for `message-chunk` / `message-complete` events keyed on
 *  the new `message_id`. The original user message + conversation
 *  are pulled from the SessionStore by the runtime — the frontend
 *  only passes the session id + intent hint. */
export async function redirectTurn(
  sessionId: string,
  intentHint: string,
): Promise<StreamStartedResponse> {
  return invoke("redirect_turn", { sessionId, intentHint });
}

/** Resume a prior session with an explicit intent. Skips router
 *  classification and streams the follow-up through the hinted intent.
 *  Used by ClarificationCard option clicks and (PR3) NextStepOffer
 *  buttons. */
export async function resumeSession(
  message: string,
  conversationId: string,
  sessionId: string,
  intentHint: string,
): Promise<StreamStartedResponse> {
  return invoke("resume_session", {
    message,
    conversationId,
    sessionId,
    intentHint,
  });
}

/** Create a new conversation, optionally tagged with the surface
 *  that owns it (`surfaceSkillId`). The tag drives routing for
 *  every subsequent turn — the workspace skill the surface declares
 *  here is the one the runtime uses to resolve `intent_policy`.
 *  Pass `undefined` for default-chat conversations.
 *
 *  See the 2026-05-24 architecture redesign in
 *  `SqliteStateStore::insert_empty_conversation` for context on why
 *  the tag lives on the conversation row instead of on global
 *  registry state. */
export async function createConversation(
  surfaceSkillId?: string,
): Promise<CreateConversationResponse> {
  return invoke("create_conversation", { surfaceSkillId });
}

/** List conversations filtered by their surface tag. Each surface
 *  passes its own `surfaceSkillId` (or `undefined` for default chat).
 *  Cross-surface visibility is structurally restricted — the default
 *  sidebar only sees default-chat conversations; Inner Work history
 *  only sees inner-work conversations; Recipe Author only sees its
 *  own. */
export async function listConversations(
  limit?: number,
  offset?: number,
  surfaceSkillId?: string,
): Promise<ConversationEntry[]> {
  return invoke("list_conversations", { limit, offset, surfaceSkillId });
}

export async function getConversation(
  conversationId: string,
): Promise<ConversationDetail> {
  return invoke("get_conversation", { conversationId });
}

export async function deleteConversation(
  conversationId: string,
): Promise<void> {
  return invoke("delete_conversation", { conversationId });
}

export async function renameConversation(
  conversationId: string,
  title: string,
): Promise<void> {
  return invoke("rename_conversation", { conversationId, title });
}

/** Persist the per-conversation corpus allow-list. `null` clears the
 *  field (= "all installed corpora", the default state). An explicit
 *  array writes the subset; pass parent corpus_ids only — retrieval
 *  expands each parent to include its layer/satellite children
 *  automatically. Called by the chip-toggle UI on every click. */
export async function setConversationEnabledCorpora(
  conversationId: string,
  enabledCorpora: string[] | null,
): Promise<void> {
  return invoke("set_conversation_enabled_corpora", {
    conversationId,
    enabledCorpora,
  });
}

export async function searchMessages(query: string): Promise<SearchResult[]> {
  return invoke("search_messages", { query });
}

/** Export a single assistant answer to a file (Markdown), carrying its
 *  citations + source ledger. `destPath` is chosen via a native save
 *  dialog on the caller side. */
export async function exportAnswer(
  conversationId: string,
  messageId: string,
  destPath: string,
): Promise<void> {
  return invoke("export_answer", {
    conversationId,
    messageId,
    destPath,
  });
}

export async function submitApproval(
  key: string,
  approved: boolean,
): Promise<boolean> {
  return invoke("submit_approval", { key, approved });
}

export async function submitInput(
  key: string,
  response: string,
): Promise<boolean> {
  return invoke("submit_input", { key, response });
}

/** Resolve a pending information-request the agent surfaced via a
 *  collaboration step. Pass `null` for content to skip; pass a string
 *  to provide pasted content. */
export async function submitInformationResponse(
  key: string,
  content: string | null,
): Promise<boolean> {
  return invoke("submit_information_response", { key, content });
}

/** Returned by `submit_information_search` so the frontend can stash
 *  the search provenance and attach it to the matching refined
 *  bubble when the post-stream refinement completes. Mirrors the
 *  Rust-side `SearchAugmentation` struct in `commands.rs`. */
export interface SearchAugmentation {
  query: string;
  backend_id: string;
  sources: Array<{ title: string; url: string }>;
  /** `false` iff the runtime had already resolved the pending
   *  request between the existence-probe and the resolve call.
   *  The UI should ignore the augmentation in that case rather
   *  than render orphaned provenance. */
  accepted: boolean;
}

/** Resolve a pending information-request by running a web search and
 *  feeding the formatted results back as if the user had pasted them.
 *  Errors when the search returns zero results (DDG bot-block etc.)
 *  so the UI can surface that without resolving the request — the
 *  card stays live for the user to paste / skip / retry.
 *
 *  `conversationId` is optional but recommended: the runtime writes
 *  a `tool_decision` outcome (Tool-Mastery Layer 3) keyed by it so
 *  the next turn's dossier surfaces the fact that the prior
 *  in-conversation lookup didn't satisfy. Without it, the outcome
 *  lands in the global tail and won't filter into this
 *  conversation's per-turn dossier pre-pass. */
export async function submitInformationSearch(
  key: string,
  query: string,
  conversationId?: string | null,
): Promise<SearchAugmentation> {
  return invoke("submit_information_search", {
    key,
    query,
    conversationId: conversationId ?? null,
  });
}

export async function listSkills(): Promise<SkillEntry[]> {
  return invoke("list_skills");
}

export async function toggleSkill(
  skillId: string,
  active: boolean,
): Promise<void> {
  return invoke("toggle_skill", { skillId, active });
}

// ─── Turn provenance (glassbox) ────────────────────────────
//
// Mirrors `sovereign_core::runtime::TurnProvenance`. The shape is
// kept flat-ish so the inner-work surface can render sections
// directly without intermediate transforms. `history_summary
// .sent_to_model` will be empty under the current streaming witness
// path — the runtime sends only the latest user message + system
// prompt, no prior turns. That emptiness is intentional surface, not
// a missing field.
export interface RecalledMemoryProv {
  id: string;
  content: string;
  created_at: number;
  /// `"raw"` for an extraction; `"summary"` for a row written by the
  /// compaction worker. Optional for backward compat: rows persisted
  /// before the compaction-fields wiring will not carry this.
  kind?: "raw" | "summary";
  /// For summaries: the ids of the source `Raw` memories this row
  /// folded. Empty (or missing) on raw memories.
  source_memory_ids?: string[];
}

export interface HistoryEntryProv {
  role: string;
  content_preview: string;
  full_chars: number;
}

export interface HistorySummaryProv {
  total_messages: number;
  user_count: number;
  assistant_count: number;
  sent_to_model: HistoryEntryProv[];
}

export interface ContradictionProv {
  prior_evidence: string;
  current_claim: string;
}

export interface TurnProvenance {
  conversation_id: string;
  message_id: string;
  captured_at: number;
  register: string;
  user_message: string;
  system_prompt: string;
  system_prompt_chars: number;
  recalled_memories: RecalledMemoryProv[];
  history_summary: HistorySummaryProv;
  temporal_tensions: string[];
  contradiction: ContradictionProv | null;
  current_goal: string | null;
  recent_topic: string | null;
  last_assistant_excerpt: string | null;
  model_id: string | null;
  max_tokens: number | null;
  enable_thinking: boolean | null;
  pass_a_ms: number | null;
}

export async function getLastTurnProvenance(
  conversationId: string,
): Promise<TurnProvenance | null> {
  return invoke("get_last_turn_provenance", { conversationId });
}

// ─── Inner-work memory ─────────────────────────────────────
//
// `finalizeInnerWorkConversation` triggers memory extraction on the
// given inner-work conversation. Called from the surface's onDestroy
// so closing the page accumulates long-term memory for future
// sessions. The runtime stamps `source_skill_id = "inner-work"` on
// each extracted memory, walling them off from general recall.
//
// `forgetMemory` soft-deletes (tombstones) a memory — recall skips
// it forever, but the row persists for audit. `weakenMemory` halves
// its confidence — still recallable but at reduced weight, and the
// standard confidence-decay floor will eventually prune it if the
// user keeps weakening.
export async function finalizeInnerWorkConversation(
  conversationId: string,
): Promise<void> {
  return invoke("finalize_inner_work_conversation", { conversationId });
}

export async function forgetMemory(memoryId: string): Promise<void> {
  return invoke("forget_memory", { memoryId });
}

export async function weakenMemory(memoryId: string): Promise<void> {
  return invoke("weaken_memory", { memoryId });
}

export async function getConfig(): Promise<DesktopConfig> {
  return invoke("get_config");
}

export async function saveConfig(config: DesktopConfig): Promise<void> {
  return invoke("save_config", { config });
}

/** Read the canonical chat-slot context window state. Settings panel
 *  uses this to render the three-value display (configured / effective
 *  / gguf-trained ceiling) — see `SetupContextWindow` in types.ts. */
export async function getSetupContextSize(): Promise<SetupContextWindow> {
  return invoke("get_setup_context_size");
}

/** Update the canonical chat-slot context window. Writes
 *  `~/.sovereign/config.toml`, kicks the daemon to reload (background),
 *  then tears down + rebuilds the desktop-embedded inference Arc.
 *  Returns when the rebuild settles — typically 15-30s on Metal. */
export async function setSetupContextSize(newCtx: number): Promise<void> {
  return invoke("set_setup_context_size", { newCtx });
}

export async function isSetupComplete(): Promise<boolean> {
  return invoke("is_setup_complete");
}

export async function completeSetup(setup: SetupConfig): Promise<void> {
  return invoke("complete_setup", { setup });
}

/** Auto-config first-launch flow. No user input — runs hardware
 *  probe, picks defaults from the bundled manifest, downloads the
 *  three model slots, opens the database, loads the model. The
 *  `setup-progress` Tauri event channel narrates progress; this
 *  promise resolves when the backend is ready to serve chat. */
export async function completeSetupAuto(primaryFile?: string): Promise<void> {
  // `primaryFile` is the user's "Customize" choice from the Setup Plan
  // screen (a catalog GGUF filename); omitted = the hardware-recommended
  // primary. Either way, the download only happens here, post-consent.
  return invoke("complete_setup_auto", { primaryFile: primaryFile ?? null });
}

/** The machine-readable setup report (`~/.sovereign/setup-report.json`) as a
 *  raw JSON string, or null if setup hasn't run yet. Powers the "What setup
 *  did" panel in Settings → About; a `setup-report.md` sits beside it on
 *  disk. */
export async function getSetupReport(): Promise<string | null> {
  return invoke("get_setup_report");
}

/** Fire-and-forget background install of the default
 *  `wikipedia-simple` corpus. Idempotent. The desktop kicks this
 *  off once the user lands in chat after first-launch setup; it
 *  runs silently with no setup-flow UI surface. */
export async function startDefaultCorpusInstall(): Promise<void> {
  return invoke("start_default_corpus_install");
}

export async function detectHardware(): Promise<HardwareInfo> {
  return invoke("detect_hardware");
}

/** Ask the backend whether a CLI-started daemon is already running
 *  and/or whether `~/.config/sovereign/config.toml` exists. The
 *  setup wizard uses this to skip the model and knowledge-tier
 *  screens when the user has already run `sovereign setup`. */
export async function detectBootstrap(): Promise<BootstrapSnapshot> {
  return invoke("detect_bootstrap");
}

export async function searchWeb(
  query: string,
  conversationId: string,
): Promise<MessageResponse> {
  return invokeChecked("search_web", { query, conversationId });
}

export async function scanForModels(): Promise<DiscoveredModel[]> {
  return invoke("scan_for_models");
}

/** Delete a GGUF model file from disk to reclaim space. Guarded on the
 *  Rust side: `.gguf` only, must live under a known model folder, and
 *  must not be assigned to a slot. Throws a human-readable error string
 *  on any guard failure. */
export async function deleteModel(path: string): Promise<void> {
  return invoke("delete_model", { path });
}

/** Returns the on-disk size of a GGUF file in bytes. Resolves to `null`
 *  when the path is empty or the file is missing — useful for the
 *  Settings → Models budget meter, which queries every slot whether or
 *  not the user has picked one. Genuine IO failures throw. */
export async function modelFileSize(path: string | null | undefined): Promise<number | null> {
  if (!path) return null;
  return invoke("model_file_size", { path });
}

/** Recommend a hardware tier for this machine. Single source of truth
 *  for tier selection lives in `sovereign-inference::hardware::select_profile`;
 *  this command is just the JSON wrapper. */
export async function recommendedProfile(): Promise<RecommendedProfile> {
  return invoke("recommended_profile");
}

/** Curated catalog of primary-model options for `profile` (or the
 *  detected profile if omitted). The headline pick has
 *  `recommended: true`; lighter alternatives follow so a user on
 *  beefy hardware can still opt into a smaller model. */
export async function primaryCatalog(
  profile?: ProfileName,
): Promise<PrimaryOption[]> {
  return invoke("primary_catalog", { profile: profile ?? null });
}

/** Single-pick recommendation for the fast or embed slot. */
export async function slotRecommendation(
  kind: "fast" | "embed",
  profile?: ProfileName,
): Promise<SlotConfig | null> {
  return invoke("slot_recommendation", { kind, profile: profile ?? null });
}

/** List the model IDs the local daemon currently advertises on
 *  `/v1/models`. Backed by a Tauri command so the request goes
 *  through reqwest on the Rust side — the renderer's `fetch` is
 *  blocked by Tauri's sandbox and fails with Safari's "Load failed". */
export async function listDaemonModels(): Promise<string[]> {
  return invoke("list_daemon_models");
}

export interface RuntimeStatus {
  members_online: number;
  members_total: number;
  pooled_vram_gb: number;
  pooled_storage_gb: number;
}

/** Pooled mesh capacity from the daemon's `/status` summary — the
 *  accurate-today numbers (free VRAM/storage across online members).
 *  Surfaced as the sidebar mesh-indicator tooltip. */
export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  return invoke("get_runtime_status");
}

export async function downloadModel(
  request: DownloadRequest,
): Promise<string> {
  return invoke("download_model", { request });
}

// ─── Corpus Management ──────────────────────────────────────

export async function listCorpora(): Promise<CorpusEntry[]> {
  return invoke("list_corpora");
}

/// Unified Library shelf listing — every installed corpus the user can
/// ask or explore, deduped into one `NotebookSummary` row each. Merges
/// the catalog/installed, local-corpus, and atlas surfaces backend-side
/// so the Library has a single source of truth (Phase 1 UX refactor).
export async function notebookList(): Promise<NotebookSummary[]> {
  return invoke("notebook_list");
}

export async function installCorpus(corpusId: string): Promise<void> {
  return invoke("install_corpus", { corpusId });
}

/// Expand an installed corpus to a relaxed scope (e.g. Wikipedia
/// Core → Full). Progress streams on the same `corpus-progress` event
/// channel as `installCorpus`.
export async function expandCorpus(corpusId: string): Promise<void> {
  return invoke("lc_expand_corpus", { corpusId });
}

/// Probe whether a corpus advertises an `expandable` scope in
/// `_corpus_meta.json`. Returns `false` for corpora that aren't
/// installed, have no filter, or have already been expanded to full.
export async function canExpandCorpus(corpusId: string): Promise<boolean> {
  return invoke("lc_can_expand", { corpusId });
}

/// Kick off the layered Wikipedia setup: Simple English first
/// (Layer 0, ready in ~2-3 min), then Wikipedia Core (Layer 1, ~10-12
/// min). Returns the list of corpus IDs that will be installed.
export async function startLayeredSetup(): Promise<string[]> {
  return invoke("lc_start_layered_setup");
}

/// Snapshot from the wikipedia-newsworthy watcher's most recent tick,
/// plus the live leader election result. Returned shape mirrors
/// `commonwealth_api::routes_internal::NewsworthyStatusResponse`.
export interface NewsworthyTickStatus {
  observed_at: number;
  node_id_str: string;
  role_leader: boolean;
  corpus_installed: boolean;
  tracked_total: number;
  owned_total: number;
  portal_ingested: boolean;
  errors: number;
  elapsed_ms: number;
  tick_interval_secs: number;
}
export interface NewsworthyStatus {
  last_tick: NewsworthyTickStatus | null;
  /// Live install state — derived from the engine at request time,
  /// not from the snapshot. Use this for "show install warning"
  /// decisions; `last_tick.corpus_installed` is the install state at
  /// last tick (can be hours stale).
  local_corpus_installed: boolean;
  leader_node_id: string | null;
  installed_peer_count: number;
  self_in_pool: boolean;
}
export async function newsworthyStatus(): Promise<NewsworthyStatus> {
  return invoke("lc_newsworthy_status");
}
/// Fire one watcher tick now. The daemon queues the work and returns
/// immediately; callers poll `newsworthyStatus()` and watch
/// `last_tick.observed_at` to confirm the tick landed.
export interface NewsworthyTickAck {
  queued: boolean;
  reason: string | null;
}
export async function newsworthyTickNow(): Promise<NewsworthyTickAck> {
  return invoke("lc_newsworthy_tick");
}

/// Generic per-corpus enrichment progress. Returned shape mirrors
/// `commonwealth_api::routes_internal::EnrichmentStatusResponse`.
/// `state` is null when no pipeline has touched the corpus yet (no
/// state file on disk). Renderable on any corpus card — watched
/// folder cards, knowledge layer chips, etc.
export type EnrichmentPhaseTag =
  | "starting"
  | "scanning"
  | "entity_extraction"
  | "raptor_leaves"
  | "raptor_tree"
  | "motif_extraction"
  | "atom_extraction"
  | "persisting"
  | "complete"
  | "failed"
  | "stalled";

export interface EnrichmentStateRow {
  schema_version: number;
  corpus_id: string;
  pipeline_id?: string;
  phase: EnrichmentPhaseTag;
  step_current: number;
  step_total: number;
  message?: string;
  started_at: number;
  last_progress_at: number;
  completed_at?: number;
  error?: string;
}

export interface EnrichmentStatus {
  corpus_id: string;
  state: EnrichmentStateRow | null;
  is_terminal: boolean;
  is_stalled: boolean;
  fraction_complete: number;
}

export async function enrichmentStatus(corpusId: string): Promise<EnrichmentStatus> {
  return invoke("lc_enrichment_status", { corpusId });
}

export async function removeCorpus(corpusId: string): Promise<number> {
  return invoke("remove_corpus", { corpusId });
}

export async function pauseCorpus(corpusId: string): Promise<void> {
  return invoke("pause_corpus", { corpusId });
}

/// Per-batch ingest throttle. `throttle_factor` ∈ (0.0, 1.0]: 1.0 =
/// full speed (the default), 0.5 ≈ duty-cycle 50% (sleep equal to
/// each embed batch's wall time after it completes — halves
/// effective throughput, leaves the GPU idle in between for chat /
/// other work). 0.0 is rejected by the daemon — use `pauseCorpus`
/// to fully stop a corpus.
export interface IngestBudgetState {
  throttle_factor: number;
}

export async function getIngestBudget(): Promise<IngestBudgetState> {
  return invokeChecked("get_ingest_budget");
}

export async function setIngestBudget(throttleFactor: number): Promise<IngestBudgetState> {
  return invokeChecked("set_ingest_budget", { throttleFactor });
}

/// Mesh-quiesce: when `true`, this node stops participating in
/// shared ingests — neither pulls peer-assigned work nor dispatches
/// its own queue to peers. Persists for the daemon's lifetime; flip
/// back via the same call. The `SOVEREIGN_DISABLE_AUTO_COLLAB` env
/// var seeds the same atomic at boot.
export interface MeshQuiesceState {
  quiesced: boolean;
}

// ─── Mesh apps (sandboxed webview apps reached via the meshapp bridge) ──

export interface MeshAppPermissions {
  mesh_store_read: boolean;
  mesh_store_write: boolean;
  inference_access: boolean;
  knowledge_access: boolean;
}

export interface MeshAppInstall {
  app_id: string;
  name: string;
  granted: MeshAppPermissions;
  trust: string;
  recorded_at_unix: number;
}

export async function listMeshApps(): Promise<MeshAppInstall[]> {
  return invoke("meshapp_list_installs");
}

export async function recordMeshAppInstall(
  appId: string,
  name: string,
  granted: MeshAppPermissions,
): Promise<MeshAppInstall> {
  return invoke("meshapp_record_install", { appId, name, granted });
}

export async function openMeshApp(appId: string): Promise<void> {
  return invoke("meshapp_open", { appId });
}

/** Open the generic Atlas Explorer mesh app bound to `corpusId` (read-only).
 *  Ensures the explorer's one-time install grant, then opens the sandboxed
 *  window over the corpus's atlas. The corpus must already be built/installed. */
export async function openCorpusExplorer(corpusId: string): Promise<void> {
  return invoke("open_corpus_explorer", { corpusId });
}

export async function uninstallMeshApp(appId: string): Promise<void> {
  return invoke("meshapp_uninstall", { appId });
}

// ─── MCP servers (Settings → MCP) ─────────────────────────────

/** One external MCP server with its live bootstrap connection status. */
export interface McpServerView {
  name: string;
  url: string;
  description: string | null;
  enabled: boolean;
  bearer: boolean;
  /** Env var the bearer token is read from (e.g. SOVEREIGN_MCP_TOKEN_VISION). */
  token_env: string | null;
  /** null = backend hasn't connected this server yet (added since last start). */
  connected: boolean | null;
  tool_count: number | null;
  error: string | null;
}

export async function listMcpServers(): Promise<McpServerView[]> {
  return invoke("mcp_list_servers");
}

export async function addMcpServer(
  name: string,
  url: string,
  description: string | null,
  bearer: boolean,
): Promise<void> {
  return invoke("mcp_add_server", { name, url, description, bearer });
}

export async function removeMcpServer(name: string): Promise<void> {
  return invoke("mcp_remove_server", { name });
}

/** Probe an MCP server without saving it — resolves to the tool count. */
export async function testMcpConnection(
  name: string,
  url: string,
  bearer: boolean,
): Promise<number> {
  return invoke("mcp_test_connection", { name, url, bearer });
}

/** A first-party mesh-app manifest (`public/meshapp/<id>/meshapp.json`) — the
 * self-describing unit a registry distributes. The host discovers apps by
 * reading these rather than hard-coding a catalog. */
export interface MeshAppManifest {
  id: string;
  name: string;
  version: string;
  blurb: string;
  /** The corpus this app reads (its data dependency). */
  corpus: string;
  /** How to acquire the corpus: its indexed size (for the UI) + the recipe
   * file the bundle ships (with a `[prebuilt]` HF snapshot block), staged into
   * the local-override recipes dir so the desktop can one-click install it. */
  corpus_data?: {
    size_indexed_gb?: number;
    recipe?: string;
  };
  /** Entry document, relative to the bundle (default `index.html`). */
  entry: string;
  /** The permission subset the app requests at install. */
  grants: MeshAppPermissions;
  /** Provenance/trust level (`unsigned` until a curated registry signs it). */
  trust?: string;
}

/** Load the available-apps catalog: the build-time index aggregated from each
 * bundle's `meshapp.json`. (Installed third-party apps will merge in via a host
 * scan in a later phase.) */
export async function loadCatalog(): Promise<MeshAppManifest[]> {
  const res = await fetch("/meshapp/catalog.json");
  if (!res.ok) throw new Error(`load meshapp catalog: ${res.status}`);
  return res.json();
}

/** Stage a mesh app's corpus recipe (shipped in its bundle) into the
 * local-override recipes dir so `installCorpus` can resolve + install it. */
export async function stageCorpusRecipe(corpusId: string, recipeToml: string): Promise<void> {
  return invoke("meshapp_stage_corpus_recipe", { corpusId, recipeToml });
}

export async function getMeshQuiesced(): Promise<MeshQuiesceState> {
  return invokeChecked("get_mesh_quiesced");
}

export async function setMeshQuiesced(quiesced: boolean): Promise<MeshQuiesceState> {
  return invokeChecked("set_mesh_quiesced", { quiesced });
}

/// Storage budget — ceiling on disk usage for corpus storage.
/// `budget_bytes = null` means no budget configured (gossip reports
/// raw free disk; nothing clamped). The daemon does the actual
/// enforcement by clamping the gossiped `free_storage_gb` to budget
/// remaining; the desktop's job is the UI surface.
export interface StorageBudgetState {
  budget_bytes: number | null;
  used_bytes: number;
  free_disk_bytes: number;
  recommended_bytes: number;
}

export async function getStorageBudget(): Promise<StorageBudgetState> {
  return invoke("get_storage_budget");
}

/// Pass `null` to clear the budget. The daemon rejects positive
/// values below 1 GiB.
export async function setStorageBudget(
  budgetBytes: number | null,
): Promise<StorageBudgetState> {
  return invoke("set_storage_budget", { budgetBytes });
}

export async function buildCorpusIndex(corpusId: string): Promise<void> {
  return invoke("build_corpus_index", { corpusId });
}

export async function diagnoseCorpus(): Promise<string> {
  return invoke("diagnose_corpus");
}

export interface IngestDocumentResult {
  source: string;
  chunks_created: number;
}

export async function ingestDocument(
  filePath: string,
): Promise<IngestDocumentResult> {
  return invoke("ingest_document", { filePath });
}

export async function getCorpusProgress(
  corpusId: string,
): Promise<CorpusProgressPayload | null> {
  return invoke("get_corpus_progress", { corpusId });
}

export async function getCorpusHealth(
  corpusId: string,
): Promise<CorpusHealthDetail | null> {
  return invoke("get_corpus_health", { corpusId });
}

export async function retryEnrichmentFailures(
  corpusId: string,
): Promise<number> {
  return invoke("retry_enrichment_failures", { corpusId });
}

// ─── Community Mesh ─────────────────────────────────────────

export async function meshCreate(
  meshName: string,
  encrypt = false,
): Promise<CreateMeshResponse> {
  return invoke("mesh_create", { meshName, encrypt });
}

export async function meshJoin(link: string): Promise<JoinMeshResponse> {
  return invoke("mesh_join", { link });
}

export async function meshPreviewJoinLink(
  link: string,
): Promise<JoinConfirmation> {
  return invoke("mesh_preview_join_link", { link });
}

export async function meshGetState(): Promise<MeshStateResponse | null> {
  return invoke("mesh_get_state");
}

export async function meshIsRunning(): Promise<boolean> {
  return invoke("mesh_is_running");
}

export async function meshLeave(): Promise<void> {
  return invoke("mesh_leave");
}

export interface RotateInviteResponse {
  mesh_name: string;
  join_key: string;
}

/** Rotate the active mesh's join key. Returns the new bare key.
 *  Existing members stay connected — only future joins use the new
 *  link. The next `meshGetState` reflects the new `join_key`/`join_link`. */
export async function meshRotateInvite(): Promise<RotateInviteResponse> {
  return invoke("mesh_rotate_invite");
}

export async function meshDiagnostics(): Promise<import("./types").MeshDiagnostics> {
  return invoke("mesh_diagnostics");
}

/** Reachable interfaces (Tailscale / LAN / IPv6) the founder can
 *  pick from when generating a remote-friendly invite. Empty list
 *  means no detected interfaces — UI hides the relay picker. */
export async function meshRelayCandidates(): Promise<
  import("./types").RelayCandidate[]
> {
  return invoke("mesh_relay_candidates");
}

/** Roll a fresh memorable node-name suggestion (e.g. "mac-peer").
 *  The 🎲 button next to the node-name input calls this; the user
 *  still has to press Save for the name to persist. */
export async function suggestNodeName(): Promise<string> {
  return invoke("suggest_node_name");
}

// ─── Mesh Health: dimensional contributions + peer preferences ──
//
// Local mode reads from the in-process daemon's contribution store.
// Attach mode currently returns empty / errors — the CLI daemon
// doesn't expose these over HTTP yet. The UI is honest about that
// gap rather than faking a value.

/** Per-peer dimensional contributions over the default 30-day window.
 *  Empty in Attach mode and when no events have accumulated. */
export async function meshGetContributions(): Promise<
  import("./types").NodeContributionsDto[]
> {
  return invoke("mesh_get_contributions");
}

/** Set the operator-private affinity multiplier this node applies to
 *  every claim it serves to `nodeId`. Multiplier must be in
 *  `(0.0, 1.0]` — the constructor rejects anything outside that
 *  range so there are no preferential lanes above neutral. */
export async function meshSetPeerPreference(
  nodeId: string,
  multiplier: number,
  reason: string | null,
): Promise<void> {
  return invoke("mesh_set_peer_preference", {
    nodeId,
    multiplier,
    reason,
  });
}

/** Clear the affinity multiplier for `nodeId`. Returns true if a
 *  preference was actually present. */
export async function meshClearPeerPreference(
  nodeId: string,
): Promise<boolean> {
  return invoke("mesh_clear_peer_preference", { nodeId });
}

/** All currently-set peer preferences. Excluded from gossip — this
 *  list never leaves the local node. */
export async function meshListPeerPreferences(): Promise<
  import("./types").PeerPreferenceDto[]
> {
  return invoke("mesh_list_peer_preferences");
}

export async function recipeValidate(
  recipePath: string,
  offline: boolean,
): Promise<RecipeValidateResult> {
  return invoke("recipe_validate", { recipePath, offline });
}

export async function recipeTest(
  recipePath: string,
  sampleSize: number,
  offline: boolean,
): Promise<RecipeTestResult> {
  return invoke("recipe_test", { recipePath, sampleSize, offline });
}

/**
 * Run the deterministic authoring harness (rungs 1–5: Acquire→Extract→Filter→
 * Chunk→Index) over a frozen sample and return the per-stage verdict ladder.
 * Model-free + offline after the first run (the frozen sample is captured once).
 */
export async function recipeRunHarness(
  recipePath: string,
  sampleSize: number,
  enrich: boolean = false,
): Promise<HarnessRunCard> {
  return invoke("recipe_run_harness", { recipePath, sampleSize, enrich });
}

// ─── Recipe authoring ("Add Knowledge Source") ─────────────

/** Result of an `Import recipe` paste/drop. When `success` is
 *  false, the recipe was NOT written and `errors` carries the
 *  validator's complaints — render them inline in the import
 *  dialog. */
export type ImportRecipeResult = {
  success: boolean;
  corpus_id: string;
  recipe_path: string;
  errors: string[];
  warnings: string[];
};

/** One declared `[parameters.<name>]` block from a recipe. Drives
 *  the install-time form: `kind` selects the input control, `default`
 *  pre-populates it, `required` flags it for validation. */
export type RecipeParameter = {
  name: string;
  kind: "string" | "int" | "date" | "list";
  description: string;
  required: boolean;
  default: unknown | null;
};

export type RecipeParameterSchema = {
  corpus_id: string;
  parameters: RecipeParameter[];
};

/** Import a recipe from a TOML string (paste or file drop). The
 *  desktop validates it and, on success, writes it under
 *  `~/.sovereign/recipes/<corpus_id>/recipe.toml` plus a registry
 *  entry. The next `listCorpora()` round-trip surfaces it as a
 *  local entry the user can install. */
export async function corpusImportRecipe(
  tomlText: string,
): Promise<ImportRecipeResult> {
  return invoke("corpus_import_recipe", { tomlText });
}

/** Read a recipe's `[parameters]` block so the UI can render an
 *  install-time form. Works for any recipe the registry can
 *  resolve — bundled, live, or locally-imported. */
export async function corpusGetRecipeParameters(
  corpusId: string,
): Promise<RecipeParameterSchema> {
  return invoke("corpus_get_recipe_parameters", { corpusId });
}

/** Same as `installCorpus`, but threads recipe parameters through
 *  to the daemon. Use for parameterized recipes (SEC EDGAR entity
 *  list, date ranges, etc.) — pass an empty `parameters` map for
 *  recipes that declare no parameters. Progress streams on the
 *  shared `corpus-progress` event. */
export async function corpusInstallWithParameters(
  corpusId: string,
  parameters: Record<string, string | number | string[]>,
): Promise<void> {
  return invoke("corpus_install_with_parameters", {
    request: { corpus_id: corpusId, parameters },
  });
}

// ─── Settings → Imports ─────────────────────────────────────

/** Outcome of `importAnthropicZip`. Tagged on `kind` — `started`
 *  means the install POST was accepted (subscribe to
 *  `corpusProgressStore.byId[corpus_id]` from here);
 *  `partial_index_exists` means the user must confirm a destructive
 *  reset before proceeding (re-invoke with `resetPartial: true`). */
export type ImportStartResponse =
  | {
      kind: "started";
      corpus_id: string;
      total_messages: number;
      estimated_minutes: number;
      canonical_path: string;
    }
  | {
      kind: "partial_index_exists";
      corpus_id: string;
      index_path: string;
      total_messages: number;
      estimated_minutes: number;
      canonical_path: string;
    };

/** Settings → Imports: unpack the Anthropic (Claude) export `.zip` the
 *  user picks, drop its `conversations.json` at the canonical landing
 *  path the `conversations-anthropic` recipe reads from, and trigger
 *  ingest.
 *
 *  Pass `resetPartial: true` after the user confirms the
 *  destructive-reset prompt. Without it, an existing partial
 *  `conversations-anthropic` index dir blocks the install and the
 *  response carries `kind: "partial_index_exists"` so the UI can
 *  show the confirmation banner. */
export async function importAnthropicZip(
  zipPath: string,
  resetPartial = false,
): Promise<ImportStartResponse> {
  return invoke("import_anthropic_zip", {
    request: { zip_path: zipPath, reset_partial: resetPartial },
  });
}

/** Settings → Imports: unpack the ChatGPT (OpenAI) export `.zip` and
 *  drive ingest of the `conversations-chatgpt` corpus. Sibling of
 *  {@link importAnthropicZip} — same request/response shape, different
 *  extractor + landing dir behind the daemon. Pass `resetPartial: true`
 *  after the user confirms the destructive-reset prompt. */
export async function importChatgptZip(
  zipPath: string,
  resetPartial = false,
): Promise<ImportStartResponse> {
  return invoke("import_chatgpt_zip", {
    request: { zip_path: zipPath, reset_partial: resetPartial },
  });
}


// ─── Insights ──────────────────────────────────────────────

export async function clipInsight(
  clippedText: string,
  messageId: string,
  paragraphIndex: number,
  sourceJson: string,
  positionJson?: string,
): Promise<InsightNodeDto> {
  return invoke("clip_insight", {
    clippedText,
    messageId,
    paragraphIndex,
    sourceJson,
    positionJson: positionJson ?? null,
  });
}

export async function listInsights(
  limit?: number,
): Promise<InsightNodeDto[]> {
  return invoke("list_insights", { limit: limit ?? null });
}

export async function searchInsights(
  query: string,
): Promise<InsightNodeDto[]> {
  return invoke("search_insights", { query });
}

export async function deleteInsight(id: string): Promise<void> {
  return invoke("delete_insight", { id });
}

export async function getSinkStatus(): Promise<SinkStatusDto> {
  return invoke("get_sink_status");
}

export async function exploreInsights(
  nodeIds: string[],
): Promise<string> {
  return invoke("explore_insights", { nodeIds });
}

// ─── Document Assets ─────────────────────────────────────────

export async function uploadDocumentAsset(
  filePath: string,
): Promise<{ asset: DocumentAsset }> {
  return invoke("upload_document_asset", { filePath });
}

export async function askDocument(
  assetId: string,
  question: string,
  conversationId: string,
): Promise<DocumentAskResponse> {
  return invoke("ask_document", { assetId, question, conversationId });
}

/** Fetch a single document asset by id. Used to pick up state changes
 *  (e.g. after an auto-heal skeleton rebuild completes). */
export async function getDocumentAsset(
  assetId: string,
): Promise<DocumentAsset | null> {
  return invoke("get_document_asset", { assetId });
}

/** Rebuild the skeleton for an asset whose ingestion was interrupted.
 *  Runs from stored chunks — no file re-upload needed. */
export async function rebuildDocumentSkeleton(
  assetId: string,
): Promise<DocumentAsset> {
  return invoke("rebuild_document_skeleton", { assetId });
}

export async function listDocumentAssets(): Promise<DocumentAsset[]> {
  return invoke("list_document_assets");
}

export async function deleteDocumentAsset(
  assetId: string,
): Promise<void> {
  return invoke("delete_document_asset", { assetId });
}

export async function listLegacyDocuments(): Promise<LegacyDocumentEntry[]> {
  return invoke("list_legacy_documents");
}

export async function promoteLegacyDocument(
  source: string,
): Promise<{ asset: DocumentAsset }> {
  return invoke("promote_legacy_document", { source });
}

// ─── Local corpus ──────────────────────────────────────────────

import type {
  LocalCorpusConfig,
  PathValidation,
  LcPreScanResponse,
  IncompleteJob,
} from "./types";

export async function lcValidatePath(path: string): Promise<PathValidation> {
  return invoke("lc_validate_path", { path });
}

export async function lcPreScan(
  path: string,
  sourceType: "folder" | "obsidian",
  displayName?: string,
): Promise<LcPreScanResponse> {
  return invoke("lc_pre_scan", {
    path,
    sourceType,
    displayName: displayName ?? null,
  });
}

/** Begin ingestion for an already-registered corpus. Returns a job_id
 *  to subscribe to `local-corpus://progress/{job_id}` on. When
 *  `withOcr` is `true`, scanned PDFs flagged by the pre-scan get
 *  rasterized + OCR'd + cleaned up via the daemon's fast slot before
 *  they're indexed. The flag persists in the corpus config so a
 *  subsequent re-ingest behaves the same way without re-prompting. */
export async function lcIngest(
  corpusId: string,
  withOcr?: boolean,
): Promise<string> {
  return invoke("lc_ingest", {
    corpusId,
    withOcr: withOcr ?? null,
  });
}

/** Whether the desktop has a working OCR pipeline (Tesseract sidecar
 *  resolved at boot). Drives the visibility of the "Read them with
 *  OCR" affordance on the pre-scan panel. */
export async function lcOcrAvailable(): Promise<boolean> {
  return invoke("lc_ocr_available");
}

export async function lcList(): Promise<LocalCorpusConfig[]> {
  return invoke("lc_list");
}

export async function lcRemove(corpusId: string): Promise<void> {
  return invoke("lc_remove", { corpusId });
}

export async function lcIncompleteJobs(): Promise<IncompleteJob[]> {
  return invoke("lc_incomplete_jobs");
}

import type { ClusterConfig as LcClusterConfig, VaultPreview as LcVaultPreview } from "./types";

export async function lcCluster(
  corpusId: string,
  config?: LcClusterConfig,
): Promise<string> {
  return invoke("lc_cluster", { corpusId, config: config ?? null });
}

export async function lcGetPreview(
  corpusId: string,
  config?: LcClusterConfig,
): Promise<LcVaultPreview> {
  return invoke("lc_get_preview", { corpusId, config: config ?? null });
}

import type {
  GitStatus as LcGitStatus,
  WriteBackResult as LcWriteBackResult,
  SnapshotMeta as LcSnapshotMeta,
  RollbackResult as LcRollbackResult,
  CleanResult as LcCleanResult,
} from "./types";

export async function lcCheckGit(corpusId: string): Promise<LcGitStatus | null> {
  return invoke("lc_check_git", { corpusId });
}

export async function lcWriteTags(
  corpusId: string,
  gitCommit: boolean,
): Promise<LcWriteBackResult> {
  return invoke("lc_write_tags", { corpusId, gitCommit });
}

export async function lcListSnapshots(corpusId: string): Promise<LcSnapshotMeta[]> {
  return invoke("lc_list_snapshots", { corpusId });
}

export async function lcRollback(
  corpusId: string,
  snapshotPath: string,
): Promise<LcRollbackResult> {
  return invoke("lc_rollback", { corpusId, snapshotPath });
}

export async function lcClean(corpusId: string): Promise<LcCleanResult> {
  return invoke("lc_clean", { corpusId });
}

export async function lcCancel(corpusId: string): Promise<boolean> {
  return invoke("lc_cancel", { corpusId });
}

// ─── Watched-folder lifecycle ─────────────────────────────────────
//
// Mirrors the daemon's `/internal/corpus/watch/*` HTTP routes; the
// Tauri commands HTTP-proxy to the running daemon (Attach mode) or
// the desktop's embedded daemon (Local mode). Same router on both
// sides, so the wire shape is identical.

import type {
  WatchedFolderConfig,
  WatchedFolderRegisterResponse,
  WatchedFolderListResponse,
  WatchedFolderStatusResponse,
  WatchedFolderStateResponse,
  WatchedFolderAckResponse,
  WatchedFolderDetailsResponse,
  WatchedFolderDocumentResponse,
  WatchedFolderIncompleteJobsResponse,
} from "./types";

export async function lcWatchRegister(
  path: string,
  displayName?: string,
  config?: WatchedFolderConfig,
  syncInitial?: boolean,
): Promise<WatchedFolderRegisterResponse> {
  return invoke("lc_watch_register", {
    path,
    displayName: displayName ?? null,
    config: config ?? null,
    syncInitial: syncInitial ?? false,
  });
}

export async function lcWatchList(): Promise<WatchedFolderListResponse> {
  return invoke("lc_watch_list");
}

export async function lcWatchStatus(
  corpusId: string,
): Promise<WatchedFolderStatusResponse> {
  return invoke("lc_watch_status", { corpusId });
}

export async function lcWatchState(
  corpusId: string,
): Promise<WatchedFolderStateResponse> {
  return invoke("lc_watch_state", { corpusId });
}

export async function lcWatchPause(
  corpusId: string,
  reason?: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_pause", { corpusId, reason: reason ?? null });
}

export async function lcWatchResume(
  corpusId: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_resume", { corpusId });
}

export async function lcWatchConfirmDeletion(
  corpusId: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_confirm_deletion", { corpusId });
}

/** Folder-ingest v1 §3.5: trigger a sweep on a Manual-mode watched
 *  folder. The daemon returns 409 (surfaced as an error here) if the
 *  corpus is in Continuous mode — the request would otherwise
 *  silently no-op. */
export async function lcWatchSyncNow(
  corpusId: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_sync_now", { corpusId });
}

/** Folder-ingest v1 §3.7: per-folder glassbox digest for the
 *  detail panel. Heavier than `lcWatchState`; fetch once when
 *  the user opens the panel rather than on every poll tick. */
export async function lcWatchDetails(
  corpusId: string,
): Promise<WatchedFolderDetailsResponse> {
  return invoke("lc_watch_details", { corpusId });
}

/** Folder-ingest v1 §3.7: per-document inspection digest for the
 *  document-inspector panel. `docId` is the relative-path key
 *  the manager stores; the Tauri command percent-encodes it. */
export async function lcWatchDocument(
  corpusId: string,
  docId: string,
): Promise<WatchedFolderDocumentResponse> {
  return invoke("lc_watch_document", { corpusId, docId });
}

/** Folder-ingest v1 §3.1: layer an additional root onto an
 *  existing watched corpus. */
export async function lcWatchAddRoot(
  corpusId: string,
  path: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_add_root", { corpusId, path });
}

/** Folder-ingest v1 §3.1: detach an additional root by 0-based
 *  index into `additional_roots`. */
export async function lcWatchRemoveRoot(
  corpusId: string,
  idx: number,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_remove_root", { corpusId, idx });
}

/** Folder-ingest v1 §3.3: enable atlas enrichment on a watched
 *  folder. Returns `{ corpus_id, job_id, ok }`. The build runs
 *  in a daemon-side subprocess; subscribe to
 *  `enrich://progress/<job_id>` for events. */
export async function lcWatchEnrichEnable(
  corpusId: string,
  pipelineId: string,
): Promise<{ corpus_id: string; job_id: string; ok: boolean }> {
  return invoke("lc_watch_enrich_enable", {
    corpusId,
    pipelineId,
  });
}

/** Folder-ingest v1 §3.3: disable atlas enrichment. Idempotent. */
export async function lcWatchEnrichDisable(
  corpusId: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_enrich_disable", { corpusId });
}

/** Folder-ingest v1 §3.3: rebuild the atlas with the previously-
 *  configured pipeline. */
export async function lcWatchEnrichRebuild(
  corpusId: string,
): Promise<{ corpus_id: string; job_id: string; ok: boolean }> {
  return invoke("lc_watch_enrich_rebuild", { corpusId });
}

export async function lcWatchRemove(
  corpusId: string,
): Promise<WatchedFolderAckResponse> {
  return invoke("lc_watch_remove", { corpusId });
}

export async function lcWatchIncompleteJobs(): Promise<WatchedFolderIncompleteJobsResponse> {
  return invoke("lc_watch_incomplete_jobs");
}

// ─── Atlas enrichment (Landing 3.C/3.D) ──────────────────────────────
//
// Wrappers for the enrichment Tauri command surface in
// sovereign-desktop/src-tauri/src/enrich_commands.rs. One function
// per command, typed end-to-end against the Rust signatures.

import type {
  EnrichBuildHandle,
  EnrichedCorpusSummary,
  PhaseFailure,
  SepIngestResult,
  EnrichEstimate,
  ActiveEnrichJob,
  StarterQuestion,
  SampledDocuments,
} from "./types";

/** Kick off an async `enrich build` run. Returns immediately with
 *  the channel name; the UI subscribes via
 *  `listen<EnrichProgress>(handle.channel, ...)`.
 *
 *  - `chapters = null` runs `--full`
 *  - `chapters = [ids]` runs `--chapters <csv>`
 *  - `skipSteps` forwards `--skip <step>` flags
 */
export async function enrichBuildAsync(
  corpusId: string,
  chapters: string[] | null,
  skipSteps: string[] | null,
): Promise<EnrichBuildHandle> {
  return invoke("enrich_build_async", {
    corpusId,
    chapters,
    skipSteps,
  });
}

// ── Run a workflow ──────────────────────────────────────────────

/** List the workflows the user can run — their own (`~/.sovereign/workflows/`)
 *  plus the shipped starters, each with the input params it declares. */
export async function workflowListRunnable(): Promise<WorkflowCatalogEntry[]> {
  return invoke("workflow_list_runnable");
}

/** Plain-language bullets of what a workflow can do (write files, use your local
 *  model, fetch the network…) — shown before a run so the user knows what it does. */
export async function workflowCapabilities(nameOrPath: string): Promise<string[]> {
  return invoke("workflow_capabilities", { nameOrPath });
}

/** Run a workflow in-process, streaming per-step progress on the returned
 *  handle's channel (`listen<WorkflowRunProgress>(handle.channel, …)`). `params`
 *  carries the whole form: folder/corpus/glob and any extra `{param.*}`. */
export async function workflowRun(
  nameOrPath: string,
  params: Record<string, string>,
): Promise<WorkflowRunHandle> {
  return invoke("workflow_run", { nameOrPath, params });
}

/** Bridge a freshly-INSTALLED recipe corpus into the atlas-enrichment path.
 *  Scaffolds the atlas config straight from the installed index
 *  (`enrich init --from-corpus`), choosing the pipeline from the recipe's
 *  `[enrichment] domain`. Call this AFTER `installCorpus` resolves and BEFORE
 *  `enrichBuildAsync` — `enrich build` requires this config (plain ingest of a
 *  `type="atlas"` text recipe runs the field-model enricher, which writes no
 *  atoms). `--force` makes it idempotent. Returns the pipeline id chosen
 *  (e.g. `"literary_atlas"`), so the UI can show what it's about to build. */
export async function recipeEnrichInitFromCorpus(
  corpusId: string,
): Promise<string> {
  return invoke("recipe_enrich_init_from_corpus", { corpusId });
}

/** Request cancellation of an in-flight build. Returns `true` if
 *  the job was found and flagged, `false` if the job_id isn't
 *  tracked (already finished or never started). Idempotent —
 *  double-clicking Cancel is harmless.
 *
 *  Typical latency to actual subprocess kill is sub-second (the
 *  CLI emits ≥ 1 stdout line per chapter). A terminal
 *  `spawn_failed` event follows carrying "Build cancelled by user". */
export async function enrichCancelBuild(jobId: string): Promise<boolean> {
  return invoke("enrich_cancel_build", { jobId });
}

/** Read the structured-failure aggregate for one corpus. Returns
 *  an array of `PhaseFailure` records the UI groups by kind. */
export async function enrichErrors(corpusId: string): Promise<PhaseFailure[]> {
  return invoke("enrich_errors", { corpusId });
}

/** Scaffold a per-article SEP enrichment corpus from the cached
 *  parquet. `paragraphsPerSection = null` uses the recipe default
 *  (5 paragraphs per section). */
export async function enrichSepIngest(
  slug: string,
  paragraphsPerSection: number | null,
): Promise<SepIngestResult> {
  return invoke("enrich_sep_ingest", {
    slug,
    paragraphsPerSection,
  });
}

/** Inventory of enrichment corpora on disk. Sorted newest-first
 *  by `created_at`. */
export async function enrichListCorpora(): Promise<EnrichedCorpusSummary[]> {
  return invoke("enrich_list_corpora");
}

/** Idempotent bridge: wrap the local-corpus staged JSONL for a
 *  folder/Obsidian ingest as a synthetic plaintext source and
 *  invoke `sovereign enrich init` against it.
 *
 *  `pipelineId` must be `literary_atlas` or `philosophy_atlas` —
 *  only atlas-producing pipelines are allowed through this path.
 *
 *  `sampleSize` optimises time-to-first-value. When set, only the
 *  first N usable records from the staged JSONL are written to the
 *  synthetic plaintext; the atlas covers that sample only. The
 *  returned `SampledDocuments.total` always reflects every usable
 *  record, so the UI can say "atlas covers 5 of your 47 documents".
 *
 *  Safe to call multiple times; if `config.json` already pins the
 *  same pipeline AND the synthetic source already covers the
 *  requested sample size, it's a no-op. */
export async function enrichInitForLocalCorpus(
  corpusId: string,
  pipelineId: "literary_atlas" | "philosophy_atlas",
  sampleSize: number | null = null,
): Promise<SampledDocuments> {
  return invoke("enrich_init_for_local_corpus", {
    corpusId,
    pipelineId,
    sampleSize,
  });
}

/** Pre-run estimate for an atlas build. Requires
 *  `enrich_init_for_local_corpus` (or equivalent) to have written
 *  `~/.sovereign/indexes/<corpus>/chapters.json`. The UI surfaces
 *  `minutes_low`..`minutes_high` as a range; the point estimates
 *  (`sections`, `est_tokens`) feed the transparency panel. */
export async function enrichEstimate(
  corpusId: string,
): Promise<EnrichEstimate> {
  return invoke("enrich_estimate", { corpusId });
}

/** If a build is currently in flight for this corpus, return the
 *  job_id + progress channel. Lets the UI attach to an existing
 *  subprocess from a different surface (e.g., the onboarding
 *  flow finds a Settings-initiated build). */
export async function enrichGetActiveJob(
  corpusId: string,
): Promise<ActiveEnrichJob | null> {
  return invoke("enrich_get_active_job", { corpusId });
}

/** Mined starter questions for the chat empty state + onboarding
 *  celebration screen. Returns an empty array when the atlas
 *  hasn't been built yet (NOT an error — the UI branches on the
 *  length to show excerpt-based fallbacks). */
export async function enrichGetStarterQuestions(
  corpusId: string,
  limit: number,
): Promise<StarterQuestion[]> {
  return invoke("enrich_get_starter_questions", { corpusId, limit });
}

/** Install the bundled "Federalist Papers" starter corpus by restoring its
 *  pre-enriched snapshot — offline, no inference, no network (~1s). Idempotent:
 *  returns `already_installed: true` if the corpus is already present. The
 *  snapshot ships as a Tauri resource and restores into the shared corpus
 *  store, so a first-time user can chat with a real, grounded corpus before
 *  authoring their own. */
export async function installStarterCorpus(): Promise<{
  corpus_id: string;
  already_installed: boolean;
}> {
  return invoke("install_starter_corpus");
}

/** True when the user has never completed the onboarding corpus
 *  flow. Checked alongside `enrichListCorpora().length === 0` in
 *  App.svelte to decide whether to gate the first-corpus flow. */
export async function isFirstRun(): Promise<boolean> {
  return invoke("is_first_run");
}

/** Write the `~/.sovereign/first_run_complete` marker so subsequent
 *  launches skip the first-corpus onboarding flow. */
export async function markFirstRunComplete(): Promise<void> {
  return invoke("mark_first_run_complete");
}

// ─── Recipe Author Workspace (M2) ────────────────────────────

import type {
  RecipeProjectListEntry,
  RecipeAuthorDashboardState,
  RecipeValidationReport,
  RestoreCheckpointOutcome,
} from "./types";

/** List recipe-author projects, newest first. Each entry carries a
 *  charter excerpt for the sidebar tooltip + summary fields driving
 *  the row state. */
export async function recipeAuthorListProjects(): Promise<
  RecipeProjectListEntry[]
> {
  return invoke("recipe_author_list_projects");
}

/** Create a new authoring project. Allocates a v4 UUID feature_id, lays down the
 *  FeatureRow + sidecar dir, returns the freshly-created list entry. `artifactKind`
 *  picks recipe vs workflow authoring (defaults to recipe; omitted → the backend's
 *  `#[serde(default)]` also yields recipe). */
export async function recipeAuthorNewProject(
  title: string,
  charterMd: string,
  artifactKind: import("./types").ArtifactKind = "recipe",
): Promise<RecipeProjectListEntry> {
  return invoke("recipe_author_new_project", {
    req: { title, charter_md: charterMd, artifact_kind: artifactKind },
  });
}

/** The single read powering the workspace dashboard. Coarse on
 *  purpose — the cards are pure presentation over slices of this
 *  struct. Polled at 2s while the workspace is open. */
export async function recipeAuthorDashboardState(
  featureId: string,
): Promise<RecipeAuthorDashboardState> {
  return invoke("recipe_author_dashboard_state", { featureId });
}

/** Validate + atomically save a hand-edited `recipe.toml` for a project,
 *  returning the SAME `RecipeValidationReport` the dashboard shows. Validate-
 *  first: a recipe that doesn't parse is NOT written (`ok=false` carries the
 *  parse errors to render inline; keep the editor text so the user can fix +
 *  re-save). On success the agent picks up the edit next turn via its disk
 *  re-read — no agent round-trip needed. */
export async function recipeAuthorSaveEditedToml(
  featureId: string,
  editedToml: string,
): Promise<RecipeValidationReport> {
  return invoke("recipe_author_save_edited_toml", {
    featureId,
    editedToml,
  });
}

/** After an authoring turn, link the artifact the agent wrote THIS turn onto the
 *  project (so the dashboard shows it). `sinceUnix` is the turn's start time, so a
 *  chat-only turn links nothing. Returns the linked artifact id, or null. */
export async function recipeAuthorLinkRecentArtifact(
  featureId: string,
  sinceUnix: number,
): Promise<string | null> {
  return invoke("recipe_author_link_recent_artifact", { featureId, sinceUnix });
}

/** Restore a project to a prior checkpoint snapshot. Lays down a new
 *  restore-anchor checkpoint and (when the project has a recipe id)
 *  overwrites the live recipe.toml from the snapshot. */
export async function recipeAuthorRestoreCheckpoint(
  featureId: string,
  checkpointId: string,
): Promise<RestoreCheckpointOutcome> {
  return invoke("recipe_author_restore_checkpoint", {
    req: { feature_id: featureId, checkpoint_id: checkpointId },
  });
}

/** Build the per-turn situated-context preamble for a Recipe Author
 *  conversation. Returns a `[Project state]…[Current recipe TOML]…
 *  [Latest validation]…[Partner says]\n` block that the chat surface
 *  concatenates with the user's message before dispatching through
 *  `sendMessageStream`. Without this, the agent has no idea which
 *  project is active and answers questions like "fix the recipe"
 *  by asking the user to paste it. */
export async function recipeAuthorBuildPrelude(
  featureId: string,
): Promise<string> {
  return invoke("recipe_author_build_prelude", { featureId });
}


// ─── Atlas Inspector (Phase 1) ───────────────────────────────

/** List every installed corpus that has an atlas on disk. Returns
 *  empty array when no atlases exist (fresh install). Sorted by
 *  corpus_id for stable rendering. */
export async function atlasListCorpora(): Promise<AtlasCorpusSummary[]> {
  return invoke("atlas_list_corpora");
}

/** Browse atoms within one corpus. Filter + paginate server-side.
 *  Pass `undefined` for filter/page to use defaults (no filter,
 *  first 200 atoms). */
export async function atlasListAtoms(
  corpusId: string,
  filter?: AtomFilter,
  page?: PageCursor,
): Promise<AtomListPage> {
  return invoke("atlas_list_atoms", {
    corpusId,
    filter,
    page,
  });
}

/** Curated landscape "Map" subgraph for a corpus — atoms as nodes (sized by
 *  salience/degree), relationships as edges (Tension edges carry their crux).
 *  Capped server-side so large corpora render as a map, not a hairball. */
export async function atlasSubgraph(
  corpusId: string,
  maxNodes?: number,
): Promise<AtlasSubgraph> {
  return invoke("atlas_subgraph", { corpusId, maxNodes });
}

/** Full inspector record for one atom — type-specific atom body,
 *  one-hop related atoms, cross-corpus bridges, and evidence
 *  excerpts. Returns `null` when the atom id isn't in the corpus
 *  (e.g., stale link after re-extraction renumbered ids). */
export async function atlasGetAtomDetail(
  corpusId: string,
  atomId: string,
): Promise<AtomDetail | null> {
  return invoke("atlas_get_atom_detail", { corpusId, atomId });
}

// ─── Conversation tiered-retrieval Atlas (A1 + A2) ────────────

/** List every conv corpus with at least one row in conv_skeletons.
 *  Parallel to atlasListCorpora but for the SQLite-backed conv
 *  enrichment surface. AtlasIndex calls both and merges. */
export async function atlasListConvCorpora(): Promise<ConvCorpusSummary[]> {
  return invoke("atlas_list_conv_corpora");
}

/** Paginated list of conversations in one corpus, filterable by
 *  case-insensitive substring on conversation overview/title. */
export async function atlasListConversations(
  corpusId: string,
  filter?: string,
  offset?: number,
): Promise<ConvListPage> {
  return invoke("atlas_list_conversations", {
    corpusId,
    filter,
    offset,
  });
}

/** Full conv detail (RAPTOR tree + state + chunk count). Returns
 *  `null` when no conv_skeletons row exists for the (corpus, conv)
 *  pair (i.e., the conv hasn't been enriched yet). */
export async function atlasGetConvDetail(
  corpusId: string,
  convUuid: string,
): Promise<ConvDetailView | null> {
  return invoke("atlas_get_conv_detail", { corpusId, convUuid });
}

/** Top-N entity chips for one conversation. Drives the chip row
 *  above ConversationChunkRenderer message bubbles. */
export async function atlasGetConvEntities(
  corpusId: string,
  convUuid: string,
): Promise<ConvEntityChip[]> {
  return invoke("atlas_get_conv_entities", { corpusId, convUuid });
}

/** Check whether the configured GliNER model is installed locally.
 *  Drives the Settings → Imports "Install model" affordance. */
export async function atlasCheckGlinerModel(): Promise<GlinerModelStatus> {
  return invoke("atlas_check_gliner_model");
}

/** Kicks off a model download. Progress streams via the
 *  `gliner-download-progress` Tauri event channel; callers subscribe
 *  with `listen("gliner-download-progress", cb)` and receive
 *  `{ file, downloaded, total }` payloads. Returns when complete. */
export async function atlasDownloadGlinerModel(
  modelId?: string,
): Promise<void> {
  return invoke("atlas_download_gliner_model", { modelId });
}

/** Aggregate one entity's footprint inside a corpus. Powers the
 *  Atlas-view entity drawer (click an `entity-chip`). Returns
 *  mention/conv counts, label breakdown, top convs, and co-occurring
 *  entities. Matches `text` case-insensitively. */
export async function atlasGetEntityAggregate(
  corpusId: string,
  text: string,
): Promise<EntityAggregateRow> {
  return invoke("atlas_get_entity_aggregate", { corpusId, text });
}

/** Per-corpus chunk-entity extraction progress. Returns `null` when
 *  extraction has never been started for this corpus. AtlasIndex
 *  polls this every ~5s while any state is non-terminal. */
export async function atlasGetChunkEntityProgress(
  corpusId: string,
): Promise<ChunkEntityProgressRow | null> {
  return invoke("atlas_get_chunk_entity_progress", { corpusId });
}

// ─── Contribution controls (W2/W3) ───────────────────────────

export interface ContributionStatus {
  /** Max concurrent peer requests; `Number.MAX_SAFE_INTEGER`-ish
   *  values mean unlimited. Compare `ceiling >= 9_000_000_000` for
   *  "no cap" UX rather than displaying a giant number. */
  ceiling: number;
  in_flight: number;
  /** Unix-seconds expiry of the active pause, or null. */
  paused_until: number | null;
  pause_remaining_secs: number | null;
  yield_peers_to_foreground: boolean;
  /** Seconds remaining in the active foreground-yield window, or null. */
  yielding_secs_remaining: number | null;
}

export interface LedgerEventDto {
  /** Hex-encoded NodeId of the origin (this node when serving). */
  node_id: unknown;
  timestamp: number;
  /** Tagged union — branch on `kind.type`:
   *  "InferenceServed" | "InferenceReceived" | "KnowledgeQueryServed"
   *  | "ShardTransferred" | "StorageSnapshot". */
  kind: { type: string; [k: string]: unknown };
}

export async function getContributionStatus(): Promise<ContributionStatus> {
  return invoke("get_contribution_status");
}

export async function setContributionCeiling(
  max: number | null,
): Promise<ContributionStatus> {
  return invoke("set_contribution_ceiling", { max });
}

export async function pauseContributions(
  durationSecs: number,
): Promise<ContributionStatus> {
  return invoke("pause_contributions", { durationSecs });
}

export async function resumeContributions(): Promise<ContributionStatus> {
  return invoke("resume_contributions");
}

export async function getRecentContributions(
  limit?: number,
): Promise<LedgerEventDto[]> {
  return invoke("get_recent_contributions", { limit: limit ?? null });
}

// ─── Activity ledger (Activity & Sharing surface) ────────────
//
// The local "what has my daemon been doing — for me and the mesh?"
// rollup. `ActivitySummary` is the daemon-side ledger (embeddings,
// ingest/enrich, local serving + folded-in peer contribution);
// `ChatActivitySummary` is the in-process chat slice derived from
// message provenance. The UI shows them together.

/** Served-work tally split by who it was for (mesh peer vs local). */
export interface ServedTally {
  local_requests: number;
  peer_requests: number;
  /** Unit count: tokens for inference, texts for embeddings. */
  local_units: number;
  peer_units: number;
}

/** Per-corpus ingest + enrich activity on this machine. */
export interface CorpusActivity {
  corpus_id: string;
  chunks_ingested: number;
  ingest_runs: number;
  ingest_seconds: number;
  enrich_runs: number;
  enrich_atoms: number;
  enrich_seconds: number;
}

export interface ActivitySummary {
  window_days: number;
  // Inference the daemon served to local API clients.
  local_inference_requests: number;
  local_tokens_generated: number;
  local_inference_wall_seconds: number;
  // Embeddings served over /v1/embeddings (peer + local).
  embeddings: ServedTally;
  // Knowledge served to local API clients.
  local_knowledge_queries: number;
  local_chunks_served: number;
  // Per-corpus ingest + enrich work done here.
  corpora: CorpusActivity[];
  total_chunks_ingested: number;
  // Newsworthy freshness fetches.
  newsworthy_fetches: number;
  newsworthy_articles: number;
  // Folded-in mesh contribution: what this node provided to peers.
  peer_inference_served_requests: number;
  peer_inference_served_tokens: number;
  peer_knowledge_queries_served: number;
  peer_bytes_served: number;
  peer_bytes_received: number;
}

export interface ChatCorpusUsage {
  origin: string;
  chunks: number;
  from_peer: boolean;
}

export interface ChatModelUsage {
  model: string;
  turns: number;
  tokens_generated: number;
}

/** The user's own chat usage, derived from persisted provenance. */
export interface ChatActivitySummary {
  window_days: number;
  turns: number;
  tokens_generated: number;
  chunks_retrieved: number;
  by_corpus: ChatCorpusUsage[];
  by_model: ChatModelUsage[];
}

/** A local-activity feed event (tagged union; branch on `kind.type`:
 *  "LocalInferenceServed" | "EmbeddingsServed" | "LocalKnowledgeServed"
 *  | "ChunksIngested" | "CorpusEnriched" | "NewsworthyFetched"). */
export interface ActivityEventDto {
  node_id: unknown;
  timestamp: number;
  kind: { type: string; [k: string]: unknown };
}

export async function getActivitySummary(
  windowDays?: number,
): Promise<ActivitySummary> {
  return invoke("get_activity_summary", { windowDays: windowDays ?? null });
}

export async function getActivityRecent(
  limit?: number,
): Promise<ActivityEventDto[]> {
  return invoke("get_activity_recent", { limit: limit ?? null });
}

export async function getChatActivity(
  windowDays?: number,
): Promise<ChatActivitySummary> {
  return invoke("get_chat_activity", { windowDays: windowDays ?? null });
}

// ─── First-mesh-join consent (W4) ────────────────────────────

export interface FirstMeshConsent {
  share_gpu: boolean;
  ceiling: number;
  recorded_at_unix: number;
}

/** Returns null when the user hasn't been prompted yet — App.svelte
 *  gates the main UI on this. */
export async function getFirstMeshConsent(): Promise<FirstMeshConsent | null> {
  return invoke("get_first_mesh_consent");
}

export async function recordFirstMeshConsent(
  shareGpu: boolean,
): Promise<FirstMeshConsent> {
  return invoke("record_first_mesh_consent", { shareGpu });
}

// ─── Crash report (W6) ───────────────────────────────────────

export interface CrashReportInfo {
  /** Absolute path of the markdown report on the user's Desktop. */
  report_path: string;
  /** The project's GitHub Issues URL — open with tauri-plugin-shell. */
  issues_url: string;
}

/** Bundles the latest supervisor crash log + redacted config into a
 *  markdown file the user can review before sending. NO auto-upload. */
export async function prepareCrashReport(): Promise<CrashReportInfo> {
  return invoke("prepare_crash_report");
}

// ─── Auto-updater ────────────────────────────────────────────
// Backed by tauri-plugin-updater. Manifest served from svrnme.sh,
// which queries GitHub Releases for the latest desktop-v* tag.
// See sovereign/crates/sovereign-desktop/RELEASING.md §Auto-updates.

export interface UpdateInfo {
  /** Version available on the server (e.g. "0.2.0"). */
  version: string;
  /** Version the running app reports (e.g. "0.1.0"). */
  current_version: string;
  /** ISO-8601 publish date, when the server provides one. */
  date: string | null;
  /** Release notes already stripped of markdown by the manifest endpoint. */
  body: string | null;
}

/** Polls the updater endpoint. Returns `null` when up to date OR on
 *  any endpoint glitch (the backend soft-fails so transient network
 *  errors don't surface as scary dialogs). */
export async function checkForUpdate(): Promise<UpdateInfo | null> {
  return invoke("check_for_update");
}

/** Downloads, verifies, installs, and restarts into the available
 *  update. Errors propagate so the UI can surface a retry path. */
export async function installUpdate(): Promise<void> {
  return invoke("install_update");
}

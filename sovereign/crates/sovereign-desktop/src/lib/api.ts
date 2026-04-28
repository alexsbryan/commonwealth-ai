import { invoke } from "@tauri-apps/api/core";
import type {
  MessageResponse,
  ConversationEntry,
  ConversationDetail,
  CreateConversationResponse,
  SearchResult,
  SkillEntry,
  DesktopConfig,
  SetupConfig,
  DiscoveredModel,
  DownloadRequest,
  CorpusEntry,
  CorpusProgressPayload,
  CorpusHealthDetail,
  HardwareInfo,
  StreamStartedResponse,
  CreateMeshResponse,
  JoinMeshResponse,
  JoinConfirmation,
  MeshStateResponse,
  RecipeValidateResult,
  RecipeTestResult,
  InsightNodeDto,
  SinkStatusDto,
  DocumentAsset,
  DocumentAskResponse,
  LegacyDocumentEntry,
  BootstrapSnapshot,
} from "./types";

export async function sendMessage(
  message: string,
  conversationId: string,
): Promise<MessageResponse> {
  return invoke("send_message", {
    message,
    conversationId,
  });
}

export async function sendMessageStream(
  message: string,
  conversationId: string,
): Promise<StreamStartedResponse> {
  return invoke("send_message_stream", {
    message,
    conversationId,
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

export async function createConversation(): Promise<CreateConversationResponse> {
  return invoke("create_conversation");
}

export async function listConversations(
  limit?: number,
  offset?: number,
): Promise<ConversationEntry[]> {
  return invoke("list_conversations", { limit, offset });
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

export async function searchMessages(query: string): Promise<SearchResult[]> {
  return invoke("search_messages", { query });
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

export async function listSkills(): Promise<SkillEntry[]> {
  return invoke("list_skills");
}

export async function toggleSkill(
  skillId: string,
  active: boolean,
): Promise<void> {
  return invoke("toggle_skill", { skillId, active });
}

export async function getConfig(): Promise<DesktopConfig> {
  return invoke("get_config");
}

export async function saveConfig(config: DesktopConfig): Promise<void> {
  return invoke("save_config", { config });
}

export async function isSetupComplete(): Promise<boolean> {
  return invoke("is_setup_complete");
}

export async function completeSetup(setup: SetupConfig): Promise<void> {
  return invoke("complete_setup", { setup });
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
  return invoke("search_web", { query, conversationId });
}

export async function scanForModels(): Promise<DiscoveredModel[]> {
  return invoke("scan_for_models");
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
  return invoke("get_ingest_budget");
}

export async function setIngestBudget(throttleFactor: number): Promise<IngestBudgetState> {
  return invoke("set_ingest_budget", { throttleFactor });
}

/// Mesh-quiesce: when `true`, this node stops participating in
/// shared ingests — neither pulls peer-assigned work nor dispatches
/// its own queue to peers. Persists for the daemon's lifetime; flip
/// back via the same call. The `SOVEREIGN_DISABLE_AUTO_COLLAB` env
/// var seeds the same atomic at boot.
export interface MeshQuiesceState {
  quiesced: boolean;
}

export async function getMeshQuiesced(): Promise<MeshQuiesceState> {
  return invoke("get_mesh_quiesced");
}

export async function setMeshQuiesced(quiesced: boolean): Promise<MeshQuiesceState> {
  return invoke("set_mesh_quiesced", { quiesced });
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

export async function meshCreate(meshName: string): Promise<CreateMeshResponse> {
  return invoke("mesh_create", { meshName });
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

/** Roll a fresh memorable node-name suggestion (e.g. "BeefyMac").
 *  The 🎲 button next to the node-name input calls this; the user
 *  still has to press Save for the name to persist. */
export async function suggestNodeName(): Promise<string> {
  return invoke("suggest_node_name");
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

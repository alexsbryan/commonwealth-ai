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

export async function removeCorpus(corpusId: string): Promise<number> {
  return invoke("remove_corpus", { corpusId });
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
 *  to subscribe to `local-corpus://progress/{job_id}` on. */
export async function lcIngest(corpusId: string): Promise<string> {
  return invoke("lc_ingest", { corpusId });
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

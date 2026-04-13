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
): Promise<DocumentAskResponse> {
  return invoke("ask_document", { assetId, question });
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

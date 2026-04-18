// ─── Command Response Types ──────────────────────────────────

export interface MessageResponse {
  message_id: string;
  role: string;
  content: string;
  task: TaskSummary | null;
  metadata?: Record<string, unknown>;
}

export interface TaskSummary {
  id: string;
  status: string;
  steps_completed: number;
}

export interface ConversationEntry {
  id: string;
  title: string | null;
  created_at: number;
  updated_at: number;
}

export interface ConversationDetail {
  id: string;
  title: string | null;
  messages: MessageEntry[];
  created_at: number;
  updated_at: number;
}

export interface MessageEntry {
  id: string;
  role: string;
  content: string;
  created_at: number;
  metadata?: Record<string, unknown>;
}

export interface CreateConversationResponse {
  id: string;
  created_at: number;
}

export interface SearchResult {
  content: string;
  conversation_id: string;
}

export interface SkillEntry {
  id: string;
  name: string;
  description: string;
  active: boolean;
  trust_level: string;
}

export interface DesktopConfig {
  model_path: string;
  primary_model_path: string | null;
  /** Optional GGUF embedding model. Required for corpus install / RAG. */
  embed_model_path: string | null;
  data_dir: string;
  skills_dir: string;
  active_skills: string[];
  enabled_tools: string[];
  context_size: number;
  search_backend: SearchBackendConfig;
  setup_complete: boolean;
  selected_tier: string | null;
  // Advanced tuning
  temperature: number;
  max_tokens: number;
  think_budget: number;
  top_k: number | null;
  /** Override for how this machine identifies itself to other mesh
   *  members. Empty string → resolved from the system hostname at
   *  mesh-create/join time. Takes effect on the next join, not
   *  retroactively. */
  node_name: string;
}

export interface SearchBackendConfig {
  provider: string;
  api_key: string | null;
}

export interface SetupConfig {
  model_path: string;
  primary_model_path?: string;
  embed_model_path?: string;
  data_dir?: string;
  active_skills: string[];
  enabled_tools: string[];
  search_provider?: string;
  search_api_key?: string;
  selected_tier?: string;
}

/** Snapshot of the desktop's bootstrap probe. Emitted by the
 *  `detect_bootstrap` Tauri command. The wizard inspects this at
 *  start to decide which screens to skip: if `cli_config_present`
 *  is true, the user has already run `sovereign setup`, so the
 *  model-path and knowledge-tier steps are covered. */
export interface BootstrapSnapshot {
  daemon_running: boolean;
  cli_config_present: boolean;
  desktop_setup_complete: boolean;
  client_port: number;
}

// ─── Event Payloads ──────────────────────────────────────────

export interface StepStartedPayload {
  task_id: string;
  step_id: number;
  description: string;
}

export interface StepDonePayload {
  task_id: string;
  step_id: number;
  description: string;
  status: string;
}

export interface ApprovalRequestPayload {
  task_id: string;
  step_id: number;
  key: string;
  tool_id: string;
  description: string;
  params: unknown;
}

export interface UserInputRequestPayload {
  task_id: string;
  key: string;
  question: string;
}

/** Sent on `information-request` when the agent suspends a research task
 *  to ask the user for a specific external piece of evidence. Renders as
 *  a dedicated card (not a chat bubble) — see InformationRequestCard.svelte. */
export interface InformationRequestPayload {
  task_id: string;
  step_id: number;
  key: string;
  current_understanding: string;
  gap: string;
  relevance: string;
  satisfying_source: string;
  search_hints: string[];
}

/** Emitted when the agent re-synthesises an already-streamed assistant
 *  message with user-supplied content. The UI replaces the message's
 *  `content` in place (identified by `message_id`). */
export interface MessageRefinedPayload {
  conversation_id: string;
  message_id: string;
  new_content: string;
}

export interface ErrorPayload {
  message: string;
}

// ─── Model Discovery & Download ─────────────────────────────

export interface DiscoveredModel {
  path: string;
  file_name: string;
  size_bytes: number;
  location_label: string;
}

export interface DownloadRequest {
  url: string;
  file_name: string;
  /** Advertised file size in GB. When present, the backend
   *  applies a 50% floor on the downloaded size via
   *  `sovereign_inference::validate_gguf` — a CDN-served HTML
   *  stub masquerading as a model file gets rejected instead of
   *  silently landing at the final path. */
  size_gb?: number;
}

export interface DownloadProgress {
  file_name: string;
  downloaded_bytes: number;
  total_bytes: number | null;
  percent: number | null;
  status: "downloading" | "complete" | "error";
  error: string | null;
}

export interface RecommendedModel {
  name: string;
  file_name: string;
  url: string;
  size_estimate: string;
  ram_minimum: string;
  description: string;
  min_ram_gb: number;
}

export interface StreamStartedResponse {
  message_id: string;
  streaming: boolean;
}

export interface MessageChunkPayload {
  conversation_id: string;
  message_id: string;
  chunk: string;
}

export interface MessageCompletePayload {
  conversation_id: string;
  message_id: string;
  full_text: string;
  metadata?: Record<string, unknown>;
}

export interface HardwareInfo {
  system_ram_gb: number;
  gpu_available: boolean;
  gpu_name: string | null;
}

// ─── Knowledge Base ─────────────────────────────────────────

export interface CorpusEntry {
  id: string;
  name: string;
  description: string;
  size_compressed_gb: number;
  size_indexed_gb: number;
  license: string;
  tiers: string[];
  status: "installed" | "installing" | "not_installed";
  chunks_count: number | null;
  /** True if the recipe enables claim/relationship enrichment (e.g. SEP). */
  enrichment_enabled: boolean;
  /** Unix timestamp (seconds) when this corpus was indexed. Null if not installed. */
  indexed_at: number | null;
  /** Embedding model name used when indexing. Null if not installed. */
  embedding_model: string | null;
  /** Embedding vector dimensions. Null if not installed. */
  embedding_dimensions: number | null;
  /** True when the IVF-PQ vector index is built and semantic search is available. */
  vector_index_ready: boolean;
}

/** Detailed health stats for an installed corpus — loaded on demand. */
export interface CorpusHealthDetail {
  corpus_id: string;
  claims_count: number;
  relationships_count: number;
  has_article_profiles: boolean;
  parse_failure_count: number;
}

export type CorpusInstallPhase =
  | "downloading"
  | "extracting"
  | "chunking"
  | "embedding"
  | "indexing"
  | "extracting_claims"
  | "finding_relationships"
  | "extracting_relationships"
  | "building_link_graph"
  | "computing_profiles"
  | "complete"
  | "failed";

export interface CorpusProgressPayload {
  corpus_id: string;
  phase: CorpusInstallPhase;
  percent: number;
  chunks_processed: number;
  message?: string;
}

// ─── Community Mesh ──────────────────────────────────────────

export interface CreateMeshResponse {
  mesh_name: string;
  join_key: string;
  join_link: string;
}

export interface JoinMeshResponse {
  mesh_name: string;
  node_id: string;
}

export interface JoinConfirmation {
  mesh_name: string;
  invited_by: string | null;
  join_key: string;
  relay_hint: string | null;
}

export type MemberStatus = "online" | "busy" | "away" | "offline";

export interface MeshStatus {
  name: string;
  members_online: number;
  members_total: number;
  model_name: string | null;
  knowledge_corpora: string[];
  is_connected: boolean;
  /** sovereign://join/cwth-... invite for the active mesh. Absent
   *  when this daemon resumed a mesh from before join_key.secret
   *  caching shipped — frontend hides the share card and offers
   *  "Rotate" to recover an inviteable link. */
  join_link?: string | null;
  /** Bare cwth-XXXX-XXXX-XXXX form, exposed in the "Or share the
   *  bare key" details so users can paste into a chat client that
   *  mangles deep-link URLs. Same caveat as join_link. */
  join_key?: string | null;
}

export interface MeshMember {
  name: string;
  node_id: string;
  is_self: boolean;
  status: MemberStatus;
  contribution_level: number; // 0-5
  contribution_label: string;
}

export type CorpusInstallStatus =
  | { type: "available" }
  | { type: "installing"; percent: number; node: string }
  | { type: "installed" }
  | { type: "shared_by_peer"; peer_name: string };

export interface MeshCorpus {
  id: string;
  name: string;
  description: string;
  article_count: string;
  download_size: string;
  // The Rust enum serializes as `{ available: null }` etc., so we accept
  // both shapes for resilience.
  status: CorpusInstallStatus | string;
}

/** A peer the daemon has spotted on the local network via mDNS.
 *  Surfaces in the MeshDiagnosticsPanel so users can verify that
 *  cross-machine LAN discovery is actually working. */
export interface DiscoveredPeerDto {
  node_id: string;
  mesh_id_hex: string;
  /** The mesh the peer claims membership in (e.g. "Masonic Mesh").
   *  Distinct from `name`, which is the node/host label. */
  mesh_name: string;
  name: string;
  address: string;
}

export interface MeshDiagnostics {
  discovered_peers: DiscoveredPeerDto[];
  daemon_running: boolean;
}

export interface ContributionSummary {
  compute_hours_contributed: number;
  compute_hours_used: number;
  storage_hosted_gb: number;
  bandwidth_served_gb: number;
  is_net_contributor: boolean;
  summary_text: string;
}

export interface MeshStateResponse {
  status: MeshStatus;
  members: MeshMember[];
  corpora: MeshCorpus[];
  contribution: ContributionSummary | null;
}

// ─── Recipe Testing ──────────────────────────────────────────

export interface RecipeValidateResult {
  passed: boolean;
  errors: string[];
  warnings: string[];
  corpus_id: string;
  corpus_name: string;
  source_reachable: boolean | null;
}

export interface RecipeTestResult {
  passed: boolean;
  warnings: string[];
  errors: string[];
  recipe_id: string;
  recipe_name: string;
  records_attempted: number;
  records_succeeded: number;
  extraction_rate: number;
  total_chunks: number;
  avg_chars: number;
  report_path: string;
  report_markdown: string;
}

// ─── UI State ────────────────────────────────────────────────

export interface TaskStep {
  id: number;
  description: string;
  status: "pending" | "running" | "done" | "skipped";
}

// ─── Insights ───────────────────────────────────────────────

export interface InsightSource {
  corpus_id: string | null;
  article_title: string | null;
  conversation_id: string;
}

export interface InsightPosition {
  name: string;
  style: PositionStyle;
}

export type PositionStyle =
  | "Compatibilism"
  | "HardIncompatibilism"
  | "Libertarianism"
  | { Custom: { bg: string; text: string; border: string } };

export type InsightSinkState =
  | "Local"
  | "PendingSync"
  | { Synced: { sink_id: string; synced_at: string } }
  | { SyncFailed: { sink_id: string; error: string } };

export interface InsightNodeDto {
  id: string;
  clipped_text: string;
  message_id: string;
  paragraph_index: number;
  source: InsightSource;
  position: InsightPosition | null;
  adjacent: string[];
  created_at: string;
  sink_state: InsightSinkState;
}

export interface SinkStatusDto {
  any_connected: boolean;
  sinks: SinkInfoDto[];
}

export interface SinkInfoDto {
  id: string;
  display_name: string;
  connected: boolean;
}

// ─── Document Assets ─────────────────────────────────────────

export interface DocumentAsset {
  id: string;
  title: string;
  filename: string;
  file_size_mb: number;
  word_count: number;
  chunk_count: number;
  document_type: string;
  ingested_at: string;
  index_id: string;
  skeleton: DocumentSkeleton | null;
  state: AssetState;
}

export type AssetState =
  | "Pending"
  | { Indexing: { chunks_done: number; chunks_total: number } }
  | "PartiallyReady"
  | { BuildingSkeleton: { chunks_done: number; chunks_total: number } }
  | "Ready"
  | { Failed: { reason: string } };

export interface DocumentSkeleton {
  sections: SectionAnnotation[];
  main_entities: RankedEntity[];
  entity_index: Record<string, EntityAppearances>;
  structural_moments: StructuralMoment[];
  overview: string;
  built_at: string;
}

export interface SectionAnnotation {
  chunk_index: number;
  function: string;
  key_entities: string[];
  establishes: string;
}

export interface RankedEntity {
  name: string;
  kind: string;
  presence_rate: number;
  first_appearance: number;
  last_appearance: number;
}

export interface EntityAppearances {
  chunk_indices: number[];
  quote_samples: string[];
}

export interface StructuralMoment {
  chunk_index: number;
  description: string;
  salience: number;
}

export type DocumentAssetOperation =
  | { Rag: { query: string } }
  | { Synthesis: { focus: string; entities: string[] } }
  | { Aggregation: { query: string } }
  | "Transformation";

export interface DocumentAskResponse {
  response: string;
  /** Absent when the question was off-topic and answered via the normal
   *  conversation pipeline — no operation badge is shown in that case. */
  operation?: DocumentAssetOperation;
  sources: string[];
}

export interface DocumentProgressPayload {
  type: string;
  asset_id?: string;
  done?: number;
  total?: number;
  main_entities?: number;
  structural_moments?: number;
  reason?: string;
}

export interface DocumentOperationPayload {
  type: string;
  operation?: string;
  name?: string;
}

export interface LegacyDocumentEntry {
  source: string;
  filename: string;
  chunk_count: number;
  word_count: number;
}

export interface DocOpProgress {
  type:
    | "Resolving"
    | "MapStarting"
    | "MapProgress"
    | "ReduceStarting"
    | "ReduceProgress"
    | "Synthesising";
  source?: string;
  chunks?: number;
  words?: number;
  total_batches?: number;
  batches_done?: number;
  fragments?: number;
  depth?: number;
}

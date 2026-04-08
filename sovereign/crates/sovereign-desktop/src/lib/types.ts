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

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
  /** Optional GGUF Code specialist. When set, `code`-hinted requests
   *  hot-swap into the lazy chat slot (shared with Main responder)
   *  instead of dispatching to primary. Null = no code slot; all
   *  substantive work goes to Main responder (pre-PR-E2 behaviour). */
  code_model_path: string | null;
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
  /** Master toggle for the KnowledgeView landscape-digest feature.
   *  When false, Sovereign skips the three enriched views
   *  (personal / conversational / institutional) + cross-view
   *  resonance, and behaves exactly as it did before KnowledgeView
   *  existed. Requires a desktop restart to take effect. Default on. */
  knowledge_view_enabled: boolean;
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

// ─── Antifragile-routing size limits ─────────────────────────

/** Mirror of `sovereign_core::runtime::MAX_TURN_MESSAGE_CHARS`.
 *  Messages larger than this get rejected by the runtime before
 *  any Fast-slot work runs. Kept in sync manually; the backend is
 *  the source of truth, this is a UX affordance so the send button
 *  can warn the user before the request fires.
 *
 *  ~16k chars ≈ 4k tokens. Document-sized content should go
 *  through the attached-file flow, which is designed for long
 *  inputs and routes through a map-reduce pipeline. */
export const MAX_TURN_MESSAGE_CHARS = 16_000;

/** Canned hint shown alongside the disabled send button. Matches
 *  the hint the runtime returns so the two surfaces read the same. */
export const OVERSIZE_MESSAGE_HINT =
  "Over 16,000 characters. For document-sized content, attach a file instead — it routes through a map-reduce pipeline designed for long inputs.";

// ─── Antifragile-routing event payloads ──────────────────────
// Mirror of sovereign-core/src/types.rs: InterpretationProposed,
// ClarificationRequest, TurnNarration, NarrationEvent. Wire format
// is JSON via Tauri events.

export type NarrationPhase =
  | "routing_committed"
  | "retrieval_complete"
  | "primary_synthesis_start"
  | "gap_check_fired";

export interface NarrationEvent {
  phase: NarrationPhase;
  text: string;
  elapsed_ms: number;
}

/** Wire payload for `interpretation-proposed`. Emitted before the
 *  first token on moderate-confidence turns. The UI renders an
 *  inline banner with `interpretation` + `alternatives` chips. */
export interface InterpretationProposedPayload {
  session_id: string;
  conversation_id: string;
  interpretation: string;
  alternatives: ProposedAlternative[];
  confidence: number;
}

export interface ProposedAlternative {
  label: string;
  intent_hint: string;
}

/** Wire payload for `clarification-request`. Emitted on low-confidence
 *  turns; the runtime suppresses synthesis until the user picks an
 *  option or types freeform input. */
export interface ClarificationRequestPayload {
  session_id: string;
  conversation_id: string;
  question: string;
  options: ClarificationOption[];
}

export interface ClarificationOption {
  label: string;
  follow_up: string;
  intent_hint: string;
}

/** Wire payload for `turn-narration`. Appended to the routing store's
 *  narrationLog. Capped at 3 per turn by the runtime. */
export interface TurnNarrationPayload {
  session_id: string;
  conversation_id: string;
  event: NarrationEvent;
}

/** PR3 — one grounded follow-up offer rendered as a clickable chip
 *  below a completed assistant message. Emitted inside
 *  `metadata.next_steps` on KnowledgeQuery turns. Clicking reuses
 *  the parent session via `resume_session` when `session_ref` is
 *  still live (<30s); otherwise the UI falls back to a fresh turn. */
export interface NextStepOffer {
  label: string;
  description?: string | null;
  follow_up_query: string;
  session_ref?: string | null;
  intent_hint?: string | null;
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

/** A reachable network address for the founder's machine, surfaced
 *  in the invite-card relay picker so they can hand a remote-friendly
 *  link to people who can't reach them via mDNS (cross-Wi-Fi,
 *  cross-subnet, AP isolation). The daemon classifies and orders
 *  these so the UI can show the best one with a "Recommended" badge. */
export interface RelayCandidate {
  /** Bare IP literal (no brackets for IPv6). */
  ip: string;
  /** "tailscale" | "lan" | "ipv6" | "other" — drives the human label
   *  and the auto-selected default. */
  kind: string;
  /** Pre-formatted "host:port" (or "[host]:port" for IPv6) ready to
   *  drop straight into the `?relay=…` query param. Saves us
   *  re-implementing IPv6 bracket rules in TS. */
  url_fragment: string;
  /** True for the single best candidate, pre-selected in the picker. */
  recommended: boolean;
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

// ─── Local corpus (Folder Drop / Obsidian) ─────────────────────────

export type LocalCorpusSourceType =
  | { ObsidianVault: { parse_frontmatter: boolean; follow_wiki_links: boolean } }
  | "DocumentFolder";

export interface WriteBackConfig {
  namespace: string;
  index_dir: string;
  snapshot_dir: string;
  snapshot_retention: number;
}

export interface WatcherConfig {
  enabled: boolean;
  debounce_ms: number;
}

export interface PreScanConfig {
  scanned_pdf_detection: boolean;
  password_detection: boolean;
  large_file_threshold_mb: number;
}

export type ChunkerKind =
  | { Paragraph: { max_chars: number; overlap_chars: number } }
  | {
      Semantic: {
        max_chars: number;
        overlap_chars: number;
        split_on_headings: number[];
      };
    };

export interface LocalCorpusConfig {
  id: string;
  display_name: string;
  root_path: string;
  source_type: LocalCorpusSourceType;
  extensions: string[];
  chunker: ChunkerKind;
  write_back: WriteBackConfig | null;
  enrichment: { enabled: boolean } | null;
  watcher: WatcherConfig;
  pre_scan: PreScanConfig;
  scope: "Local" | "Mesh" | "Public";
}

export interface FileMeta {
  path: string;
  size_bytes: number;
  display_name: string;
}

export interface PreScanResult {
  readable: FileMeta[];
  scanned_pdfs: FileMeta[];
  protected_pdfs: FileMeta[];
  corrupt_files: FileMeta[];
  large_files: FileMeta[];
  ignored_types: number;
  total_visited: number;
}

export interface PathValidation {
  exists: boolean;
  is_dir: boolean;
  readable: boolean;
  canonical_path: string | null;
}

export interface LcPreScanResponse {
  job_id: string;
  result: PreScanResult;
  corpus_id: string;
  display_name: string;
}

export interface RuntimeFailure {
  file: FileMeta;
  reason: string;
}

export interface ExcerptChunk {
  text: string;
  source_name: string;
  page_ref: string | null;
}

export interface IngestStats {
  corpus_id: string;
  files_indexed: number;
  chunks_written: number;
  runtime_failures: RuntimeFailure[];
  excerpt_chunks: ExcerptChunk[];
  duration_secs: number;
}

export interface IncompleteJob {
  corpus_id: string;
  display_name: string;
  files_done: number;
  files_total: number;
}

/// Mirror of Rust's `LocalCorpusProgress` with `tag = "phase"`, `content = "data"`.
export type LocalCorpusProgress =
  | { phase: "scanning"; data: { done: number; total: number } }
  | {
      phase: "staging";
      data: { done: number; total: number; current_file: string };
    }
  | {
      phase: "ingesting";
      data: {
        done: number;
        total: number;
        phase_label: string;
        current_file: string | null;
      };
    }
  | {
      /** Page-level progress within the OCR pipeline for a scanned
       *  PDF. Emitted before each page is sent to the OCR engine. */
      phase: "ocr_page";
      data: {
        file: string;
        page: number;
        total_pages: number;
        file_idx: number;
        file_total: number;
      };
    }
  | { phase: "clustering"; data: { stage: ClusterStage } }
  | { phase: "snapshotting"; data: { done: number; total: number } }
  | { phase: "writing"; data: { done: number; total: number } }
  | { phase: "rolling_back"; data: { done: number; total: number } }
  | { phase: "complete"; data: { result: IngestStats } }
  | {
      phase: "error";
      data: { message: string; recoverable: boolean };
    };

export type ClusterStage = {
  stage:
    | "embedding_matrix"
    | "hdbscan_run"
    | "llm_labeling"
    | "open_question_detection";
};

// ─── Clustering + Preview (Obsidian M4) ────────────────────────────

export type MultiClusterStrategy = "Dominant" | "All" | "Flag";

export interface ClusterConfig {
  min_cluster_size: number;
  min_confidence: number;
  multi_tag_threshold: number;
  multi_cluster_strategy: MultiClusterStrategy;
  /** Minimum distinct notes per cluster after the chunk-to-note
   *  rollup. Clusters with fewer notes than this collapse — their
   *  notes land in the outlier panel with reason `singleton_cluster`.
   *  Default 2: a tag shared by only one note feels premature. */
  min_notes_per_cluster: number;
}

export interface LabeledCluster {
  id: number;
  tag_path: string;
  display_name: string;
  description: string;
  note_count: number;
  centroid_chunk_ids: number[];
}

export interface FileAssignment {
  chunk_id: number;
  relative_path: string;
  note_title: string;
  primary_tag: string;
  additional_tags: string[];
  confidence: number;
  existing_tags: string[];
}

export interface ClusterSummary {
  cluster: LabeledCluster;
  assignments: FileAssignment[];
}

export interface ClusterConfidence {
  cluster_id: number;
  confidence: number;
}

export type OutlierReason =
  | { type: "low_confidence"; threshold: number }
  | { type: "ambiguous_cluster"; top_clusters: ClusterConfidence[] }
  | { type: "too_short"; char_count: number }
  | { type: "singleton_cluster"; cluster_size: number };

export interface OutlierNote {
  chunk_id: number;
  relative_path: string;
  note_title: string;
  best_cluster_id: number;
  best_cluster_confidence: number;
  reason: OutlierReason;
}

export interface OpenQuestion {
  gap_description: string;
  relevant_cluster_ids: number[];
}

export interface FlaggedNote {
  chunk_id: number;
  note_title: string;
  candidate_clusters: ClusterConfidence[];
}

export interface VaultPreview {
  clusters: ClusterSummary[];
  outliers: OutlierNote[];
  flagged: FlaggedNote[];
  total_notes: number;
  tagged_notes: number;
  outlier_count: number;
  open_questions: OpenQuestion[];
  namespace: string;
}

// ─── WriteBack / Snapshots (Obsidian M5) ───────────────────────────

export interface GitStatus {
  current_branch: string;
  has_uncommitted_changes: boolean;
}

export interface FailedWrite {
  relative_path: string;
  reason: string;
}

export interface WriteBackResult {
  files_tagged: number;
  files_skipped: FailedWrite[];
  index_notes_created: number;
  snapshot_path: string;
  sovereign_version: number;
}

export interface SnapshotMeta {
  taken_at: string;
  sovereign_version: number;
  file_count: number;
  git_commit: string | null;
  snapshot_path: string;
}

export interface RollbackResult {
  files_restored: number;
  files_skipped: FailedWrite[];
  index_notes_deleted: number;
}

export interface CleanResult {
  tags_removed_from: number;
  index_notes_deleted: number;
}

// ─── Atlas enrichment (Landing 3.C) ──────────────────────────────────
//
// Types mirror the Rust side 1:1 — see:
//   corpus-engine/src/enrichment/pipeline/progress.rs   (EnrichProgress, BuildStep)
//   corpus-engine/src/enrichment/pipeline/types.rs      (PhaseFailure, PhaseFailureKind)
//   sovereign/crates/sovereign-desktop/src-tauri/src/enrich_commands.rs
//
// Rust uses `#[serde(tag = "kind", rename_all = "snake_case")]` with
// flat fields per variant (NOT the `tag`+`content` shape
// LocalCorpusProgress uses). Keep these TS unions keyed on `kind`
// and flat-fielded; a tag-shape mismatch would silently wire the
// UI to events it can't route.

/// A build step. Values match `BuildStep::id()` on the Rust side.
export type EnrichBuildStep =
  | "seed"
  | "extract"
  | "cluster"
  | "name"
  | "resolve"
  | "tensions"
  | "gaps"
  | "configure"
  | "report";

/// Progress event streamed on `enrich://progress/{job_id}` during
/// an `enrich_build_async` run. The UI listens with
/// `listen<EnrichProgress>(channel, handler)`.
export type EnrichProgress =
  | {
      kind: "build_start";
      corpus_id: string;
      pipeline_id: string;
      steps: EnrichBuildStep[];
      auto_skipped: EnrichBuildStep[];
    }
  | {
      kind: "step_start";
      corpus_id: string;
      step: EnrichBuildStep;
      ordinal: number;
      total: number;
    }
  | {
      kind: "chapter_progress";
      corpus_id: string;
      chapter_id: string;
      index: number;
      total: number;
      question_count: number | null;
    }
  | {
      kind: "chapter_failed";
      corpus_id: string;
      chapter_id: string;
      /// `PhaseFailureKind` snake_case id (e.g. "parse_drift").
      failure_kind: string;
      reason: string;
    }
  | {
      kind: "step_done";
      corpus_id: string;
      step: EnrichBuildStep;
      summary: string;
    }
  | {
      kind: "step_failed";
      corpus_id: string;
      step: EnrichBuildStep;
      message: string;
      exit_code: number;
    }
  | {
      kind: "complete";
      corpus_id: string;
      steps_completed: number;
    }
  | {
      kind: "aborted";
      corpus_id: string;
      failed_step: EnrichBuildStep;
      exit_code: number;
    }
  | {
      /// The build couldn't start at all — CLI binary not on
      /// $PATH, permission denied on spawn, etc. Distinct from
      /// "aborted" because no step ran; the UI surfaces this as
      /// "couldn't start build" rather than attributing it to
      /// a specific step.
      kind: "spawn_failed";
      corpus_id: string;
      message: string;
    }
  | {
      /// User-initiated cancellation killed the build. Distinct
      /// from aborted/spawn_failed so the UI can render
      /// "Cancelled" without string-sniffing messages.
      kind: "cancelled";
      corpus_id: string;
      at_step: EnrichBuildStep | null;
    };

/// Handle returned by `enrich_build_async`. The UI uses `channel`
/// directly with `listen` — it already encodes the job id so
/// components don't need to reconstruct `enrich://progress/{id}`
/// themselves.
export interface EnrichBuildHandle {
  job_id: string;
  corpus_id: string;
  channel: string;
}

/// One entry in the `enrich_list_corpora` response. `created_at`
/// is an ISO-8601 UTC string; the panel sorts newest-first.
export interface EnrichedCorpusSummary {
  corpus_id: string;
  pipeline_id: string;
  source_path: string;
  created_at: string;
}

/// Return type for `enrich_sep_ingest`. `log` carries the CLI's
/// stdout so the UI can render it inside a collapsible audit
/// panel — operators see exactly what was scaffolded.
export interface SepIngestResult {
  corpus_id: string;
  slug: string;
  log: string;
}

/// Returned by `enrich_init_for_local_corpus`. Lets the UI say
/// "Ready to ask about {titles} — X of Y documents covered" after
/// the sample-first atlas build finishes.
export interface SampledDocuments {
  /// Up to `sample_size` document titles actually written to the
  /// synthetic source. In ingest-walker order (deterministic).
  titles: string[];
  /// Every usable record present in the staged JSONL at init time.
  /// `titles.length < total` means this is a sample build.
  total: number;
}

/// Pre-run estimate for an atlas build. `minutes_low`..`minutes_high`
/// is the range the onboarding UI surfaces; `sections` + `est_tokens`
/// power the transparency panel ("we'll process N documents").
export interface EnrichEstimate {
  sections: number;
  total_words: number;
  est_tokens: number;
  minutes_low: number;
  minutes_high: number;
}

/// Returned by `enrich_get_active_job` when a build is in flight
/// for this corpus. Absent when no job is running. Lets one UI
/// surface attach to a subprocess another surface kicked off.
export interface ActiveEnrichJob {
  job_id: string;
  channel: string;
}

/// One starter question mined from an atlas's Question atoms by
/// `enrich_get_starter_questions`. Rendered as a chip in the
/// onboarding celebration screen + ChatView empty state. Clicking
/// a chip pre-fills + auto-submits the chat input.
export interface StarterQuestion {
  /// Already normalised to end with `?`.
  text: string;
  /// Provenance pointer — stable atom id (e.g. "question-0003").
  atom_id: string;
  /// First `raised_at` chunk id (section id, usually). `null` when
  /// the atom has no passage reference.
  source_section: string | null;
  /// Snake-case QuestionType tag ("thematic", "interpretive", …).
  question_type: string;
}

/// One structured failure record from `enrich_errors`. The UI
/// groups these by `(phase, kind)` and renders the `remediation`
/// string (populated by the CLI's `--json` path from
/// `PhaseFailureKind::remediation_hint()` on the Rust side).
export interface PhaseFailure {
  /// `PipelinePhase::id()` on the Rust side — snake_case
  /// ("questions", "atlas_named_clusters", "tensions", …).
  phase: string;
  /// Prefix-tagged subject: "chapter:sec_0001",
  /// "sketch:entity_state:sec_0003#2", "cluster:claim:cl_c_01", …
  subject: string;
  /// `PhaseFailureKind` snake_case id.
  kind: string;
  reason: string;
  raw_response_head?: string | null;
  /// One-line remediation hint the UI shows next to the group
  /// header. Populated by the CLI's `--json` view; absent only if
  /// an older CLI ships without the view wrapper.
  remediation?: string;
}

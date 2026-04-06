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
  data_dir: string;
  skills_dir: string;
  active_skills: string[];
  enabled_tools: string[];
  context_size: number;
  search_backend: SearchBackendConfig;
  setup_complete: boolean;
  selected_tier: string | null;
}

export interface SearchBackendConfig {
  provider: string;
  api_key: string | null;
}

export interface SetupConfig {
  model_path: string;
  primary_model_path?: string;
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
  size_indexed_gb: number;
  license: string;
  tiers: string[];
  status: "installed" | "installing" | "not_installed";
  chunks_count: number | null;
  trust_level: string;
}

export interface CorpusProgressPayload {
  corpus_id: string;
  phase: "downloading" | "parsing" | "complete" | "failed";
  percent: number;
  chunks_processed: number;
}

// ─── UI State ────────────────────────────────────────────────

export interface TaskStep {
  id: number;
  description: string;
  status: "pending" | "running" | "done" | "skipped";
}

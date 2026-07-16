// SPDX-License-Identifier: AGPL-3.0-or-later
// ─── Command Response Types ──────────────────────────────────

/// Structured error returned by migrated Tauri command handlers (§2D-3).
/// Mirrors the Rust `DesktopError` wire shape exactly (see
/// `src-tauri/src/error.rs`, pinned by a serialization test there);
/// `invokeChecked` normalises every rejection to this so callers branch
/// on `code` instead of parsing error strings.
export type ErrorCode = "not_ready" | "invalid_request" | "upstream" | "internal";

export interface DesktopError {
  code: ErrorCode;
  message: string;
  suggested_action: string;
}

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
  /** User-controlled per-conversation corpus allow-list. `null` (the
   *  default for fresh / legacy conversations) means "all installed
   *  corpora participate in retrieval". An explicit array restricts
   *  retrieval + the model's prompt list to those parent corpus_ids;
   *  layer/satellite corpora follow their parent. Updated via
   *  `setConversationEnabledCorpora`. */
  enabled_corpora?: string[] | null;
}

export interface MessageEntry {
  id: string;
  role: string;
  content: string;
  created_at: number;
  metadata?: Record<string, unknown>;
  /** True between the moment the user clicks "Search the web" / submits
   *  paste content on the InformationRequestCard and the moment
   *  MESSAGE_REFINED arrives. AssistantMessage uses this to render a
   *  "Refining…" indicator on the bubble so the in-place rewrite is
   *  not a surprise. Cleared by MESSAGE_REFINED (success) or a
   *  refinement-error event. */
  refining?: boolean;
  /** Set on the refined bubble when the refinement was sourced from
   *  the search-now affordance. Drives the "Augmented via web
   *  search: <query> (N sources)" footer note. Persisted into
   *  `metadata.search_augmentation` so it survives hydration. */
  searchAugmentation?: SearchAugmentation;
}

/** Mirrors `SearchAugmentation` in `commands.rs` / `api.ts`. */
export interface SearchAugmentation {
  query: string;
  backend_id: string;
  sources: Array<{ title: string; url: string }>;
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

/** Behavioural family of a loaded model. Drives chat-template /
 *  tokenizer / pooling quirks. PascalCase mirrors the Rust
 *  `ModelFamily` enum (`#[serde(rename_all = "PascalCase")]`). */
export type ModelFamily =
  | "Qwen3"
  | "Qwen35"
  | "Qwen3Embedding"
  | "Gemma3"
  | "Gemma4"
  | "Llama3"
  | "Phi4"
  | "Phi4Reasoning"
  | "SmolLM3"
  | "Reranker"
  | "Unknown";

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
  /** Model family for the code slot. Drives chat-template / tokenizer
   *  quirks. `"Qwen35"` for Qwen-Coder lineage, `"Unknown"` (default)
   *  for BYOM coders. */
  code_family: ModelFamily;
  data_dir: string;
  skills_dir: string;
  active_skills: string[];
  enabled_tools: string[];
  /** **Deprecated.** Kept for backwards compat with desktop.toml files
   *  written before the SetupConfig merge. The canonical home is now
   *  `~/.svrnmesh/config.toml`'s `[models].context_size`, surfaced via
   *  the `get_setup_context_size` / `set_setup_context_size` Tauri
   *  commands. Settings UI reads/writes through those; this field
   *  exists only so existing TOMLs deserialise without losing the
   *  pre-merge value during one-shot migration. */
  context_size?: number | null;
  search_backend: SearchBackendConfig;
  setup_complete: boolean;
  selected_tier: string | null;
  /** When true, `knowledge_lookup` auto-escalates to the configured
   *  web-search provider if the local envelope returns thin results.
   *  Off by default — every web call goes through the
   *  InformationRequest card. */
  auto_escalate_to_web: boolean;
  // Advanced tuning
  temperature: number;
  max_tokens: number;
  think_budget: number;
  top_k: number | null;
  /** Epistemic-humility audit. When true, the runtime inspects each
   *  draft for thin evidence and may surface an InformationRequest
   *  card. Default on. */
  auto_collaborate: boolean;
  /** Naked mode — run the loaded model raw, with none of the svrnmesh
   *  affordances (retrieval, router, grounding gate, tools, atlas, gap
   *  check). Chat history → model → reply, with only a minimal assistant
   *  preamble + custom_instructions. Default off. */
  naked_mode: boolean;
  /** Shared-model cluster role: "consumer" (use a mesh-hosted shared model as
   *  primary), "anchor" (lend memory to hold it), or "host" (own the loaded
   *  instance). Default "consumer". */
  shared_model_role?: "consumer" | "anchor" | "host";
  /** The shared model id to use/host (as advertised in the mesh). Null = not
   *  participating in a shared model. */
  shared_model_id?: string | null;
  /** User-authored "custom instructions" / persona. Global standing
   *  guidance appended as the outermost layer of every system prompt —
   *  append-only, never replacing the situated context. Null/empty is a
   *  no-op. Visible verbatim in the Inner Work ProvenancePanel. */
  custom_instructions?: string | null;
  /** Idle seconds before the lazy chat slot (primary + code) is
   *  unloaded to reclaim memory. Mirrors
   *  `DaemonSection::primary_idle_secs`; default 300. */
  primary_idle_secs: number;
  /** Embedding model family. `"Qwen3Embedding"` for qwen3-embedding-*
   *  GGUFs (last-token pooling); `"Unknown"` (default) for mxbai and
   *  similar mean-pooling embedders. Wrong family silently produces
   *  incompatible vectors. */
  embed_family: ModelFamily;
  /** Override for how this machine identifies itself to other mesh
   *  members. Empty string → resolved from the system hostname at
   *  mesh-create/join time. Takes effect on the next join, not
   *  retroactively. */
  node_name: string;
  /** Master toggle for the KnowledgeView landscape-digest feature.
   *  When false, svrnmesh skips the three enriched views
   *  (personal / conversational / institutional) + cross-view
   *  resonance, and behaves exactly as it did before KnowledgeView
   *  existed. Requires a desktop restart to take effect. Default on. */
  knowledge_view_enabled: boolean;
  /** Persisted disk-usage ceiling for corpus storage. `null` = auto
   *  (compute at boot from free disk; persisted on first launch). */
  storage_budget_bytes: number | null;
  /** M3 — opt-in for the Recipe Author workspace. When false (the
   *  default), the workspace switcher is hidden in the chat sidebar.
   *  Toggled from Settings → Advanced. */
  enable_recipe_authoring: boolean;
  /** Opt-in: serve the phone-facing sovereign-server API so the svrnmesh
   *  mobile app can pair over the tailnet. The host delegates inference to the
   *  daemon (no second model load). Toggled from Settings → Mobile access. */
  mobile_access_enabled: boolean;
}

/** Snapshot of the canonical chat-slot context window state, sourced
 *  from `~/.svrnmesh/config.toml` plus the loaded gguf's
 *  `n_ctx_train`. Returned by the `get_setup_context_size` Tauri
 *  command; the Settings panel renders the triple side-by-side so the
 *  user can see configured vs. effective vs. gguf-ceiling. */
export interface SetupContextWindow {
  /** Value persisted in `~/.svrnmesh/config.toml`'s
   *  `[models].context_size`, or the daemon-side default (16384) when
   *  no explicit value is set. Editable. */
  configured: number;
  /** The value the running primary slot was actually built with —
   *  what `clamp_max_tokens` and the runtime budget against. Usually
   *  equals `configured` (post llama-cpp 256-byte pad). May differ
   *  immediately after a hot reload until the next inference call
   *  refreshes the slot's resident state. `null` when the active
   *  provider doesn't own a local slot (remote API forwarding). */
  effective: number | null;
  /** GGUF-declared `n_ctx_train` for the primary slot's model. The
   *  upper bound llama.cpp will silently cap `configured` at without
   *  a RoPE-scaling rebuild. `null` when the active provider doesn't
   *  own a local model. */
  n_ctx_train: number | null;
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
  /** M3 — opt-in for the Recipe Author workspace. `undefined` from a
   *  wizard step that doesn't surface the toggle preserves the
   *  existing config value rather than silently defaulting to false. */
  enable_recipe_authoring?: boolean;
  mobile_access_enabled?: boolean;
}

/** Snapshot of the desktop's bootstrap probe. Emitted by the
 *  `detect_bootstrap` Tauri command. The wizard inspects this at
 *  start to decide which screens to skip: if `cli_config_present`
 *  is true, the user has already run `svrn setup`, so the
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

/** Discriminates the two producers of an information-request card.
 *  Mirrors `sovereign_core::types::InformationRequestKind` (snake-case
 *  serde rename). The UI renders distinct chrome per kind: `refinement`
 *  cards are post-answer "would source X sharpen this?" prompts
 *  anchored to the most recent assistant bubble; `step_block` cards
 *  represent a paused task waiting on the user for a specific input
 *  and carry `task_title`. */
export type InformationRequestKind = "refinement" | "step_block";

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
  /** Producer discriminator. See `InformationRequestKind`. */
  kind: InformationRequestKind;
  /** Populated only for `step_block` cards; empty string for
   *  `refinement`. The card renders this as "Task: <goal>" in the
   *  header. */
  task_title: string;
}

/** Emitted when the agent re-synthesises an already-streamed assistant
 *  message with user-supplied content. The UI replaces the message's
 *  `content` in place (identified by `message_id`). */
export interface MessageRefinedPayload {
  conversation_id: string;
  message_id: string;
  new_content: string;
}

// ─── TEACHABLE lessons (coach in chat, own in settings) ───────

/** Enforcement rung — where a lesson's intent is honored (TEACHABLE §7).
 *  NEVER render these raw tokens; the pane maps them to user language
 *  ("answer length" / "wording check" / "standing reminder"). */
export type LessonEnforcement = "param" | "transform" | "prompt";

/** Provenance: the teaching moment a lesson was saved from. */
export interface LessonTaughtFrom {
  excerpt: string;
  conversation_id: string;
  message_id: string;
}

/** Sent on `lesson-proposed` when a durative coaching turn produced a
 *  draft lesson. Carries the FULL draft so consent is stateless: Save
 *  passes this payload (display possibly edited) to `save_lesson`;
 *  "Not this" calls nothing and nothing is stored. Mirrors
 *  `sovereign_core::types::LessonProposedPayload` — `taught_from` is
 *  the verbatim excerpt STRING on this wire shape (the saved row
 *  carries the structured `LessonTaughtFrom`). */
export interface LessonProposedPayload {
  /** Draft uuid — journal correlation key, not the eventual note id. */
  id: string;
  conversation_id: string;
  message_id: string;
  display: string;
  prompt_form: string;
  enforcement: LessonEnforcement;
  params: Record<string, unknown>;
  taught_from: string;
}

/** One row from `list_lessons` — payload fields flattened with note
 *  lifecycle fields. `retired_at != null` means superseded (render
 *  struck-through); the SUCCESSOR row's `supersedes` points here, so
 *  "replaced by" resolves client-side via
 *  `rows.find(r => r.supersedes === row.id)`. */
export interface LessonRow {
  id: string;
  display: string;
  prompt_form: string;
  enforcement: LessonEnforcement;
  params: Record<string, unknown>;
  scope: string[];
  taught_from: LessonTaughtFrom;
  enabled: boolean;
  created: number;
  first_applied_at: number | null;
  last_affirmed: number | null;
  /** Pre-edit draft sentence when the user edited the card before
   *  saving — the consented correction pair (TEACHABLE §11). */
  drafted_display: string | null;
  retired_at: number | null;
  retired_by: string | null;
  supersedes: string | null;
}

/** Shape of `metadata.kept_lesson` — stamped by the runtime exactly
 *  once, on the first message a saved lesson influenced. Renders as
 *  the one-time "Kept: <rule>" whisper footer, then never again. */
export interface KeptLesson {
  id: string;
  display: string;
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

// NarrationPhase mirrors `sovereign_core::types::NarrationPhase`. Serde's
// default external tagging serialises unit variants as bare strings
// ("routing_committed") and struct variants as a one-key object
// ({"tool_invocation_start": {...}}). Consumers must handle both shapes.
export type NarrationPhase =
  // Unit variants — serialised as bare strings.
  | "routing_committed"
  | "retrieval_complete"
  | "primary_synthesis_start"
  | "grounding_verify_start"
  | "gap_check_fired"
  | "lesson_drafted"
  | "routing_start"
  | "retrieval_start"
  | "curation_start"
  | "drafting_start"
  | "presentation_start"
  // Struct variants — serialised as `{key: payload}` objects.
  | { routing_complete: { intent: string; register: string; confidence: number } }
  // `top_titles` is `#[serde(default)]` on the Rust side — optional here
  // so older recordings and test fixtures without it still typecheck.
  | {
      retrieval_complete: {
        chunks_in: number;
        corpora: string[];
        top_titles?: string[];
      };
    }
  | {
      curation_complete: {
        chunks_kept: number;
        skeleton: string[];
        sufficient: boolean;
      };
    }
  | { drafting_complete: { tokens: number; finish_reason: string } }
  | { presentation_complete: { judge_score: number | null } }
  | { stage_error: { stage: string; error: string } }
  | {
      tool_invocation_start: {
        call_id: string;
        tool_id: string;
        summary: string;
      };
    }
  | {
      tool_invocation_complete: {
        call_id: string;
        tool_id: string;
        ok: boolean;
        result_summary: string;
      };
    }
  // Live synthesis heartbeat — the running held-token count while the
  // grounding gate holds the answer. Rendered as ONE live chip that the
  // store REPLACES (not appends) on each event; see routing.svelte.ts.
  | { synthesis_progress: { tokens: number } }
  // EXPERIMENT (SOVEREIGN_DRAFT_STREAM=1): incremental UNVERIFIED draft
  // text during the gated hold. Accumulated into `draftPreview` (never
  // the narration log) and rendered as a visually-provisional section
  // that collapses when the gated answer lands. The affordance contract:
  // draft text must never be styled as final.
  | { draft_delta: { delta: string } }
  // ── Grounding-gate claim-check frames (the verification counter) ──
  // Live per-claim progress from the gate ladder. Routed to the
  // `counter` context field (never the narration log — see
  // `applyCounter` in routing.machine.ts) and rendered by CounterCard.
  // Two-frame contract on claim_check_start: an empty-claims frame the
  // moment the audit opens, then a frame carrying the extracted list.
  | { claim_check_start: { claims: string[]; recheck: boolean } }
  | { claim_verdict: { index: number; supported: boolean } }
  | { claim_revision_start: { failed: number } }
  | { claim_check_complete: { confirmed: number; flagged: number } };

export interface NarrationEvent {
  phase: NarrationPhase;
  text: string;
  elapsed_ms: number;
}

/** Discriminator: returns the snake_case tag whether `phase` is a bare
 *  string (unit variant) or a one-key object (struct variant). */
export function narrationPhaseTag(phase: NarrationPhase): string {
  if (typeof phase === "string") return phase;
  const keys = Object.keys(phase);
  return keys[0] ?? "unknown";
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

/** `message-error` payload from the streaming send / redirect / resume
 *  paths (`commands/chat.rs`). Carries conversation_id + message_id so a
 *  turn that fails while the user is viewing a DIFFERENT conversation is
 *  still attributable — and recoverable from the live-turns registry on
 *  return — rather than silently vanishing. The generic `error` /
 *  `backend-error` events keep using the leaner `ErrorPayload`. */
export interface MessageErrorPayload {
  conversation_id: string;
  message_id: string;
  message: string;
}

export interface HardwareInfo {
  system_ram_gb: number;
  gpu_available: boolean;
  gpu_name: string | null;
  /** Discrete GPU VRAM in GB. Null on unified-memory (Apple Silicon) or no GPU. */
  gpu_memory_gb: number | null;
  /** True on Apple Silicon (M-series). Drives unified-vs-discrete tiering. */
  is_unified_memory: boolean;
}

/** Hardware-tier names from `models.toml`. Mirrors
 *  `sovereign_inference::hardware::ProfileName`, serialised by the
 *  desktop's `recommended_profile` command as the bare manifest keys. */
export type ProfileName =
  | "cpu_only"
  | "low_mem"
  | "default"
  | "high"
  | "very_high";

/** Returned by `recommendedProfile()`. Effective memory is unified RAM
 *  on Apple Silicon, GPU VRAM on discrete cards, otherwise system RAM. */
export interface RecommendedProfile {
  profile: ProfileName;
  effective_memory_gb: number;
  is_unified_memory: boolean;
}

/** One row in the daemon-supplied primary-model catalog. Sourced from
 *  `models.toml` via `setup_planner::build_primary_catalog`. */
export interface PrimaryOption {
  /** `ProfileName` this slot was drawn from — the headline pick has
   *  `recommended: true`; lighter alternatives are surfaced beneath. */
  profile: ProfileName;
  recommended: boolean;
  file: string;
  base_name: string;
  family: string;
  quant: string;
  size_gb: number;
  /** Repo URL from the manifest. */
  hf_url: string;
  /** Direct GGUF download URL — already includes
   *  `/resolve/main/<file>`. Use this with `downloadModel`. */
  download_url: string;
}

/** Single-pick slot (fast or embed). Same fields as `PrimaryOption`
 *  minus the catalog metadata. */
export interface SlotConfig {
  file: string;
  base_name: string;
  family: string;
  quant: string;
  size_gb: number;
  hf_url: string;
  download_url: string;
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
  /**
   * If set, this corpus is a layer/satellite of `parent_corpus_id`.
   * The picker hides children from the top-level list and renders
   * them as toggles under the parent's row instead.
   */
  parent_corpus_id: string | null;
  /** Catalog presentation tier from `registry_snapshot.toml`.
   *  - `"featured"` — robustly built; install affordance enabled.
   *  - `"preview"` — recipe declared but ingest pipeline not ready;
   *    rendered under "Coming soon" with install disabled.
   *  - `"hidden"` — never surfaced in the catalog picker (dev-only
   *    corpora or auto-managed recipes).
   *  `null` falls back to `"preview"` so newly-registered recipes
   *  surface under Coming soon by default. */
  catalog_status: string | null;
}

/** One row on the Library shelf — the unified, deduped view of an
 *  *installed* corpus the user can ask or explore. Assembled by the
 *  `notebook_list` command, which merges `installed_indexes()` (the
 *  deduped on-disk set), the local-corpus configs (source kind + display
 *  name + scope), and the atlas readers (explorable flag). Mirrors the
 *  Rust `NotebookSummary`. */
export interface NotebookSummary {
  /** Corpus id — the citation handle. */
  id: string;
  /** Human-facing name (local display name → catalog name → index name → id). */
  name: string;
  /** `"folder"` | `"obsidian"` | `"watched"` | `"catalog"` | `"installed"`. */
  source_kind: string;
  /** Chunk count from the installed index. */
  doc_count: number;
  /** True when the corpus has an explorable map on disk (atoms.json or
   *  conv-tiered enrichment). Drives the ✦ badge and the Explore tab. */
  explorable: boolean;
  /** Index build time (Unix seconds) — the freshness signal. */
  updated_unix: number | null;
  /** `"local"` | `"mesh"` | `"public"`. */
  scope: string;
  /** Count of open (unadjudicated) conflicts for a governance corpus.
   *  `null` for an ordinary corpus — which is what gates the Conflicts
   *  tab off; `0` still shows the tab (exports + "all clear"). */
  open_conflicts: number | null;
}

/** Notebook source kinds, narrowed for the shelf's icon + label map.
 *  Backend may emit other strings; the UI treats unknowns as `"installed"`. */
export type NotebookSourceKind =
  | "folder"
  | "obsidian"
  | "watched"
  | "catalog"
  | "installed";

// ── Governance (FR-9) — mirrors corpus-engine `GovernanceView` +
//    the desktop `governance_commands` payloads. The Rust enums are
//    serde-internally-tagged (`RuleStatus` on `status`, `TensionDisposition`
//    on `disposition`, `GovernanceIssue` on `issue`), snake_case; these TS
//    discriminated unions must key on the same tag. `OpId` / `AtomId` /
//    `EdgeId` serialize as bare strings (newtype transparent).

/** A governed rule's status in current law. */
export type RuleStatus =
  | { status: "active" }
  | { status: "superseded"; by: string; by_rules: string[] }
  | { status: "retracted"; by: string };

/** How a surfaced conflict stands — open, adjudicated, or moot. */
export type TensionDisposition =
  | { disposition: "open" }
  | { disposition: "resolved"; by: string }
  | { disposition: "accepted"; by: string }
  | { disposition: "dismissed"; by: string }
  | { disposition: "moot"; dead_endpoint: string };

/** Glass-box data-integrity finding — surfaced, never silently dropped. */
export type GovernanceIssue =
  | { issue: "rule_has_no_atom"; rule: string }
  | { issue: "tension_endpoint_missing"; tension: string; endpoint: string }
  | { issue: "adjudicated_tension_not_surfaced"; tension: string }
  | { issue: "unattended_act"; op: string };

/** A rule's source citation. `chunk_id` is a *section* id (e.g.
 *  `"sec_00001"`); resolve via `GovernanceViewPayload.section_chunks` for
 *  a deep-link, or show `passage_preview` inline (zero I/O). */
export interface ChunkRef {
  chunk_id: string;
  passage_preview?: string;
  source_doc_id?: string;
}

/** A governed rule with its derived status, ready to render. */
export interface RuleView {
  id: string;
  text: string;
  /** `"requires"` | `"forbids"` | `"permits"`. */
  deontic?: string;
  /** Scope entity id — the topic this rule governs. */
  scope?: string;
  citation?: ChunkRef;
  status: RuleStatus;
}

/** A surfaced conflict with both rule texts attached — a meeting-agenda row. */
export interface TensionView {
  id: string;
  rule_a: string;
  text_a: string;
  rule_b: string;
  text_b: string;
  /** The sub-question the conflict turns on (the crux). */
  why?: string;
  confidence: number;
  disposition: TensionDisposition;
}

/** The joined governance read-model. */
export interface GovernanceView {
  rules: RuleView[];
  tensions: TensionView[];
  issues: GovernanceIssue[];
}

/** Recipe vocabulary labels so the panel speaks the community's language. */
export interface OntologyVocabulary {
  position_term: string | null;
  tension_term: string | null;
  concern_term: string | null;
  evidence_term: string | null;
}

/** Everything the Conflicts panel needs, from `governance_get_view`. */
export interface GovernanceViewPayload {
  view: GovernanceView;
  /** section id → human title (e.g. `"Decision — 2026-03-14"`). */
  section_titles: Record<string, string>;
  /** citation section id → numeric chunk id, for "view passage" deep-links. */
  section_chunks: Record<string, number>;
  /** scope entity id → canonical name, for topic grouping in exports. */
  scope_names: Record<string, string>;
  vocabulary: OntologyVocabulary | null;
  /** op id → decision metadata (timestamp, rationale, actor), to sort
   *  settled decisions recent-first and show *why* each was made. */
  decisions: Record<string, DecisionMeta>;
  /** Documents changed since the last atlas build → show the "update" banner. */
  docs_changed_since_build: boolean;
}

/** Metadata for one governance decision, keyed by op id. */
export interface DecisionMeta {
  ts_unix: number;
  /** Human rationale (empty for an auto-asserted rule). */
  rationale: string;
  /** `"human:<name>"` or `"seed"` — who authored the act. */
  actor: string;
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
  /** Total chunks the current phase expects (0/absent when unknown).
   *  With `chunks_per_sec`, drives the glassbox ETA. */
  chunks_total?: number;
  /** Live embedding throughput (chunks/sec, 0/absent when unknown). */
  chunks_per_sec?: number;
  message?: string;
}

// ─── Community Mesh ──────────────────────────────────────────

export interface CreateMeshResponse {
  mesh_name: string;
  join_key: string;
  join_link: string;
  /** Bearer token a remote app/script must present to this machine's
   *  API. Present once the mesh is shared (daemon bound non-loopback);
   *  absent for a loopback-only daemon. */
  client_token?: string | null;
}

export interface JoinMeshResponse {
  mesh_name: string;
  node_id: string;
  client_token?: string | null;
}

export interface JoinConfirmation {
  mesh_name: string;
  invited_by: string | null;
  join_key: string;
  relay_hint: string | null;
  /** Founder's iroh dial string — present when the invite carries a
   *  no-VPN connect path (either mesh kind). `encrypted` says which. */
  iroh_dial: string | null;
  /** True iff the invite is for an ENCRYPTED mesh (fail-closed
   *  key-dialed join). False with `iroh_dial` present = a plaintext
   *  mesh reachable over iroh (prefer-iroh join, IP/mDNS fallback). */
  encrypted?: boolean;
  /** Unix-seconds TTL after which the invite is rejected (display). */
  expires_at: number | null;
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

// ─── Mesh Health: dimensional contributions + peer preferences ──

export interface CorpusHostingDto {
  corpus_id: string;
  corpus_name: string;
  size_gb: number;
  queries_served: number;
  is_sole_host: boolean;
}

export interface NodeContributionsDto {
  node_id: string;
  window_days: number;
  inference_served_requests: number;
  inference_served_tokens: number;
  inference_served_wall_seconds: number;
  inference_consumed_requests: number;
  inference_consumed_tokens: number;
  corpora_hosted: CorpusHostingDto[];
  bytes_served: number;
  bytes_received: number;
}

export interface PeerPreferenceDto {
  node_id: string;
  /** Multiplier in (0.0, 1.0] applied to every claim affinity in
   *  the manifest served to the corresponding peer. Constructor
   *  rejects values outside this range — `1.0` means no
   *  adjustment, lower values reduce affinity, never above 1.0
   *  (no preferential lanes). */
  multiplier: number;
  reason: string | null;
  set_at: number;
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
  /** Bearer token for remote apps/scripts connecting to this machine's
   *  API. Present on a shared mesh; null when loopback-only. Rendered
   *  on the active-mesh invite card beside the join key. */
  client_token?: string | null;
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

// ─── Authoring harness (deterministic verdict ladder) ────────
// Mirrors `sovereign_authoring_harness::HarnessRun` + the desktop
// `HarnessRunCard` Tauri return. The harness runs the REAL pipeline
// stages over a frozen sample and emits a Pass/Fail/Warn verdict per
// stage with the failing items shown, not summarized.

export type HarnessStatus = "pass" | "fail" | "warn";

export interface HarnessLocus {
  kind: "doc" | "chunk" | "atom";
  id: string;
}

export interface HarnessEvidenceItem {
  locus: HarnessLocus;
  excerpt: string;
}

export interface HarnessVerdict {
  check: string;
  status: HarnessStatus;
  /** What the declaration promised — threshold always on screen. */
  expected: string;
  /** What actually happened. */
  observed: string;
  /** Concrete failing/sample items — never just a count. */
  evidence: HarnessEvidenceItem[];
}

export interface HarnessStageResult {
  stage: string;
  config_hash: string;
  cache_hit: boolean;
  verdicts: HarnessVerdict[];
}

export interface HarnessRun {
  sample_id: string;
  recipe_hash: string;
  stages: HarnessStageResult[];
}

export interface HarnessRunCard {
  /** Roll-up: all stages pass → green; any fail → red. Warns never gate. */
  green: boolean;
  run: HarnessRun;
  ran_at_unix: number;
  /** Frozen-sample provenance — "❄ Frozen: N docs". */
  frozen_docs: number;
  frozen_captured_at: number;
  /** True when this call performed the one networked capture step. */
  frozen_captured_now: boolean;
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
  | "MultiHopReady"
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
  /** The persisted assistant-message metadata, verbatim — provenance +
   *  retrieved_chunks (document-op path) or the runtime turn's full
   *  metadata incl. `grounding_gate` (fallback path). Merged into the
   *  live bubble so it renders identically to a reload. */
  metadata?: Record<string, unknown>;
}

export interface DocumentProgressPayload {
  type: string;
  asset_id?: string;
  done?: number;
  total?: number;
  /** Present on the `Started` event — seeds the progress bar's
   *  denominator before the first `Indexing` tick arrives. */
  chunk_count?: number;
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
  | "DocumentFolder"
  | { WatchedFolder: WatchedFolderConfig };

/** Folder-ingest v1 §3.5 sync cadence policy. Mirrors
 *  `sovereign_tools::local_corpus::config::SyncMode`. */
export type SyncMode = "continuous" | "manual";

/** Per-corpus tunables for a `WatchedFolder` source. Mirrors
 *  `sovereign_tools::local_corpus::config::WatchedFolderConfig`. */
export interface WatchedFolderConfig {
  follow_symlinks: boolean;
  deletion_guard: DeletionGuardConfig;
  /** Floor: 60s. Default 120. Ignored when `sync_mode === "manual"`. */
  sweep_interval_secs: number;
  /** Default 7 days. */
  soft_delete_grace_secs: number;
  exclude_globs: string[];
  /** OCR scanned PDFs (no text layer). Requires the daemon's
   *  OcrCtx to be installed — `lcOcrAvailable()` reflects whether
   *  the runtime can honour the toggle. Default false. */
  with_ocr: boolean;
  /** Folder-ingest v1 §3.5: `"continuous"` (default) sweeps on the
   *  scheduler tick. `"manual"` opts out of periodic sweeps; the
   *  corpus only sweeps when an explicit `lcWatchSyncNow` request
   *  flips the per-state pending flag. */
  sync_mode: SyncMode;
  /** Folder-ingest v1 §3.4: when `true`, the corpus is excluded
   *  from the agent's ambient situated-context assembly. The folder
   *  remains searchable on explicit query and via Inner Work mode.
   *  Default `false`. */
  sensitive: boolean;
  /** Folder-ingest v1 §3.1: additional roots layered on top of
   *  the primary `LocalCorpusConfig.root_path`. Empty for single-
   *  root corpora (the default). The walker iterates the primary
   *  first, then each additional in declared order. */
  additional_roots: WatchedFolderRootSpec[];
  /** Folder-ingest v1 §3.3: per-folder atlas enrichment opt-in.
   *  Default `Off`. */
  enrichment: WatchedEnrichmentConfig;
}

/** One additional root attached to a watched-folder corpus. Mirrors
 *  `sovereign_tools::local_corpus::config::RootSpec`. */
export interface WatchedFolderRootSpec {
  path: string;
  added_at_unix: number;
}

export interface DeletionGuardConfig {
  absolute_threshold: number;
  fractional_threshold: number;
  enabled: boolean;
}

export const DEFAULT_WATCHED_FOLDER_CONFIG: WatchedFolderConfig = {
  follow_symlinks: false,
  deletion_guard: {
    absolute_threshold: 100,
    fractional_threshold: 0.25,
    enabled: true,
  },
  sweep_interval_secs: 120,
  soft_delete_grace_secs: 7 * 86_400,
  exclude_globs: [],
  with_ocr: false,
  sync_mode: "continuous",
  sensitive: false,
  additional_roots: [],
  enrichment: { kind: "off" },
};

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

// ─── Watched-folder status ─────────────────────────────────────────

/** Tagged union mirroring `WatchedFolderStatus` on the Rust side.
 *  The `kind` discriminator matches `serde(tag = "kind")`. */
export type WatchedFolderStatus =
  | { kind: "idle"; last_sweep_unix: number; live_docs: number; tombstones: number }
  | { kind: "sweeping"; phase: SweepPhase; current: number; total: number }
  | {
      kind: "paused_awaiting_confirmation";
      diff_summary: DiffSummary;
      tripped_rule: TrippedRule;
      sweep_started_unix: number;
    }
  | { kind: "paused_manual"; since_unix: number; reason: string }
  | { kind: "errored"; message: string; errored_unix: number };

export type SweepPhase =
  | "walking"
  | "diffing"
  | "deleting"
  | "updating"
  | "adding"
  | "gc_soft_deletes";

export interface DiffSummary {
  added: number;
  modified: number;
  removed: number;
  live_before: number;
}

export type TrippedRule =
  | { rule: "absolute"; threshold: number; observed: number }
  | { rule: "fractional"; threshold: number; observed: number };

export interface FailedFile {
  doc_id: string;
  absolute_path: string;
  /** "corrupt" | "password_protected" | "scanned_no_text" — extensible. */
  kind: string;
  reason: string;
  first_seen_unix: number;
}

// ─── Watched-folder Tauri command response shapes ──────────────────

export interface WatchedFolderRegisterResponse {
  corpus_id: string;
  display_name: string;
  initial_sweep:
    | { kind: "skipped" }
    | { kind: "spawned"; corpus_id: string }
    | { kind: "completed"; files_indexed: number; chunks_written: number };
}

export interface WatchedFolderListEntry {
  corpus_id: string;
  display_name: string;
  root_path: string;
  status: WatchedFolderStatus;
  /** Folder-ingest v1 §3.5: `"continuous"` (default) sweeps periodically;
   *  `"manual"` opts out and waits for `lcWatchSyncNow`. */
  sync_mode: SyncMode;
  /** Folder-ingest v1 §3.4: when `true`, this folder is excluded from
   *  ambient situated-context assembly. */
  sensitive: boolean;
  /** Folder-ingest v1 §3.1: count of additional roots layered on
   *  top of the primary `root_path`. `0` for single-root corpora.
   *  The list card surfaces "+N folders" when non-zero so the
   *  user can spot multi-root setups without opening the
   *  detail panel. */
  additional_roots_count: number;
}

export interface WatchedFolderListResponse {
  corpora: WatchedFolderListEntry[];
}

export interface WatchedFolderStatusResponse {
  corpus_id: string;
  status: WatchedFolderStatus;
}

/** Folder-ingest v1 §3.3 — per-folder enrichment status surfaced
 *  in the detail panel. Mirrors the `EnrichmentStatus` enum on
 *  `sovereign-mesh::corpus_watch_http`. */
export type EnrichmentStatus =
  | { kind: "off" }
  | {
      kind: "building";
      pipeline_id: string;
      phase: string;
      current: number;
      total: number;
      started_at_unix: number;
    }
  | {
      kind: "complete";
      pipeline_id: string;
      built_at_unix: number;
      doc_count: number;
      /** Live entry count at request time. Compute "M new docs
       *  since last build" as `current_doc_count - doc_count`. */
      current_doc_count: number;
    }
  | {
      kind: "failed";
      pipeline_id: string;
      failed_at_unix: number;
      reason: string;
    };

/** Folder-ingest v1 §3.3 — per-folder atlas enrichment opt-in.
 *  Mirrors `WatchedEnrichmentConfig` in
 *  `sovereign_tools::local_corpus::config`. */
export type WatchedEnrichmentConfig =
  | { kind: "off" }
  | {
      kind: "on";
      pipeline_id: string;
      last_built_at_unix: number;
      last_built_doc_count: number;
    };

/** Folder-ingest v1 §3.7 — glassbox folder-detail digest returned
 *  by `lcWatchDetails`. Mirrors `DetailsResponse` in
 *  `sovereign-mesh/src/corpus_watch_http.rs`. */
export interface WatchedFolderDetailsResponse {
  corpus_id: string;
  display_name: string;
  root_path: string;
  status: WatchedFolderStatus;
  sync_mode: SyncMode;
  sensitive: boolean;
  live_entries: number;
  /** Per-extension count of indexed documents. Keyed by lowercase
   *  extension (e.g. `"pdf"`, `"md"`); files without an extension
   *  bucket as `"(no extension)"`. */
  formats: Record<string, number>;
  /** Per-extension count of files the walker saw but skipped
   *  because no extractor was registered for that extension. */
  skipped_by_extension: Record<string, number>;
  failed_files: WatchedFailedFile[];
  tombstones: number;
  enrichment: EnrichmentStatus;
  last_sweep_unix: number;
  /** Folder-ingest v1 §3.1 multi-root: every root attached to
   *  this corpus (primary first, then each additional in
   *  declared order). Always at least 1 entry. */
  roots: WatchedFolderRoot[];
}

/** One root attached to a watched-folder corpus. `idx === 0` is
 *  the primary; `idx >= 1` map onto `additional_roots[idx - 1]`. */
export interface WatchedFolderRoot {
  idx: number;
  path: string;
  added_at_unix: number;
  doc_count: number;
  primary: boolean;
}

export interface WatchedFailedFile {
  doc_id: string;
  absolute_path: string;
  /** Reason kind: `"corrupt"`, `"password_protected"`,
   *  `"scanned_no_text"`, etc. The detail panel groups by this
   *  for the §3.7 "What I don't have" surface. */
  kind: string;
  reason: string;
  first_seen_unix: number;
}

/** Folder-ingest v1 §3.7 — per-document inspection digest
 *  returned by `lcWatchDocument`. Mirrors `DocumentResponse` in
 *  `sovereign-mesh/src/corpus_watch_http.rs`. */
export interface WatchedFolderDocumentResponse {
  corpus_id: string;
  doc_id: string;
  absolute_path: string;
  size_bytes: number;
  mtime_unix: number;
  content_hash: string;
  /** Number of chunks the engine has indexed for this document.
   *  Zero means the file failed extraction or hasn't been swept yet. */
  chunk_count: number;
  /** First chunk's content, truncated to ~500 chars. `null` when
   *  `chunk_count === 0`. */
  first_chunk_preview: string | null;
  atoms: WatchedFolderDocumentAtom[];
}

export interface WatchedFolderDocumentAtom {
  atom_id: string;
  atom_type: string;
  label: string;
}

export interface WatchedFolderStateResponse {
  corpus_id: string;
  status: WatchedFolderStatus;
  skipped_by_extension: Record<string, number>;
  failed_files: FailedFile[];
  tombstones: number;
  live_entries: number;
}

export interface WatchedFolderAckResponse {
  corpus_id: string;
  ok: boolean;
}

export interface WatchedIncompleteJob {
  corpus_id: string;
  display_name: string;
  root_path: string;
  status: WatchedFolderStatus;
  tombstones: number;
  failed_files: number;
}

export interface WatchedFolderIncompleteJobsResponse {
  jobs: WatchedIncompleteJob[];
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

// ── Run a workflow ──────────────────────────────────────────────
// Mirror `WorkflowParamSpec` / `WorkflowCatalogEntry` / `WorkflowRunHandle` /
// `WorkflowRunEvent` in `workflow_commands.rs`.

/// One input a workflow declares (`{param.key}`). `kind` lets the run form
/// render a dedicated control for folder/corpus/glob and a text box otherwise.
export interface WorkflowParamSpec {
  key: string;
  kind: "folder" | "corpus" | "glob" | "text";
  label: string;
}

/// A runnable workflow + the inputs it needs. `origin` is
/// `"shipped:<name>"` or `"user:<name>"`.
export interface WorkflowCatalogEntry {
  name: string;
  description: string;
  origin: string;
  params: WorkflowParamSpec[];
}

/// Handle returned by `workflow_run`. The UI listens on `channel` with
/// `listen<WorkflowRunProgress>(channel, handler)`. `corpus` is the corpus the
/// run will build (if any), for the "chat with it" handoff.
export interface WorkflowRunHandle {
  job_id: string;
  channel: string;
  corpus: string | null;
}

/// One progress event from a `workflow_run`, tagged on `kind`. The terminal
/// events are `complete` (with the built corpus, if any) and `failed`.
export type WorkflowRunProgress =
  | { kind: "run_started"; workflow: string; items: number; steps: number }
  | {
      kind: "step_done";
      item: string;
      step: string;
      uses: string;
      for_each: boolean;
      cached: boolean;
      step_index: number;
      total_steps: number;
    }
  | { kind: "element_skipped"; item: string; step: string; index: number; error: string }
  | { kind: "item_done"; item: string; ok: boolean; ran: number; cached: number }
  | { kind: "run_finished"; ok: number; failed: number }
  | { kind: "complete"; ok: number; failed: number; corpus: string | null }
  | { kind: "failed"; error: string };

/// One entry in the `enrich_list_corpora` response. `created_at`
/// is an ISO-8601 UTC string; the panel sorts newest-first.
export interface EnrichedCorpusSummary {
  corpus_id: string;
  pipeline_id: string;
  source_path: string;
  created_at: string;
}

/// Document-coverage summary the folder flow renders after ingest —
/// "Ready to ask about {titles} — X of Y documents covered". Now
/// populated locally by `FolderDropFlow` from ingest stats (the old
/// `enrich_init_for_local_corpus` command that returned it was
/// removed with the CLI-shell enrichment path).
export interface SampledDocuments {
  /// Representative document titles to name in the UI. May be empty
  /// when the flow only knows a count.
  titles: string[];
  /// Total usable documents covered by the build.
  total: number;
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

// ─── Recipe Author Workspace (M2) ────────────────────────────

/** Sidebar entry for one recipe-author project. */
/** What an authoring project builds. The recipe-author workspace hosts both —
 *  a recipe (corpus ingest) or a workflow (a step pipeline). Mirrors the Rust
 *  `ArtifactKind` (serde `snake_case`). */
export type ArtifactKind = "recipe" | "workflow";

/** Lowercase noun for prose ("recipe" | "workflow"). Tolerates the field being
 *  absent (older seeded payloads) → "recipe". */
export function artifactNoun(kind: ArtifactKind | null | undefined): string {
  return kind === "workflow" ? "workflow" : "recipe";
}

/** Capitalized label for headings ("Recipe" | "Workflow"). */
export function artifactTitle(kind: ArtifactKind | null | undefined): string {
  return kind === "workflow" ? "Workflow" : "Recipe";
}

export interface RecipeProjectListEntry {
  feature_id: string;
  title: string;
  /** First ~200 chars of the charter — sidebar tooltip. */
  charter_excerpt: string;
  /** recipe | workflow. Optional for back-compat with pre-tag payloads (→ recipe). */
  artifact_kind?: ArtifactKind;
  recipe_id?: string | null;
  current_sample_size?: number | null;
  last_test_status?: string | null;
  /** Unix seconds. */
  created_at: number;
  updated_at: number;
}

/** One feature-scoped note (decision / research / capability /
 *  issue / deferred-question) with payload pre-parsed for the UI. */
export interface DashboardNoteEntry {
  id: string;
  kind: string;
  content: string;
  /** RFC 3339. */
  created_at: string;
  decision_kind?: string | null;
  attribution?: string | null;
  /** Parsed payload_json — null for legacy rows. */
  payload?: unknown;
}

/** On-disk metadata for one project checkpoint. */
export interface RecipeCheckpointMeta {
  checkpoint_id: string;
  name: string;
  trigger: string;
  summary?: string;
  /** Set when this checkpoint was created via restore. Carries the
   *  source checkpoint id. */
  restored_from?: string | null;
  /** RFC 3339 timestamp. */
  created_at: string;
}

/** Result of running the on-disk recipe.toml through the engine's
 *  parser. The dashboard's `RecipeValidationCard` renders this so a
 *  partner sees "your recipe doesn't parse — here's why" without
 *  having to read agent tool output. `errors` is human-readable and
 *  already carries the engine's translate_parse_error rewrites. */
export interface RecipeValidationReport {
  ok: boolean;
  errors: string[];
  /** True when the project hasn't drafted a recipe yet — distinguishes
   *  "nothing to validate" from "we tried and it failed". */
  no_recipe: boolean;
  /** True when the recipe parsed AND its enrichment will produce graph atoms
   *  (enabled atlas/investigation). False for a valid recipe whose enrichment
   *  is off or field_model — it would build to ZERO atoms. */
  enrichment_ready: boolean;
}

/** Single coarse read for the workspace dashboard. */
export interface RecipeAuthorDashboardState {
  feature_id: string;
  title: string;
  charter_md: string;
  /** recipe | workflow — drives the dashboard's labels + the chat surface's
   *  skill tag. Optional for back-compat with pre-tag payloads (→ recipe). */
  artifact_kind?: ArtifactKind;
  recipe_id?: string | null;
  recipe_path?: string | null;
  recipe_toml?: string | null;
  current_sample_size?: number | null;
  last_test_status?: string | null;
  last_test_at?: string | null;
  /** Unix seconds. */
  created_at: number;
  updated_at: number;
  decisions: DashboardNoteEntry[];
  research_findings: DashboardNoteEntry[];
  capability_requests: DashboardNoteEntry[];
  recipe_issues: DashboardNoteEntry[];
  deferred_questions: DashboardNoteEntry[];
  checkpoints: RecipeCheckpointMeta[];
  validation: RecipeValidationReport;
}

export interface RestoreCheckpointOutcome {
  new_checkpoint_id: string;
  source_checkpoint_id: string;
}

// ─── Atlas Inspector (Phase 1) ───────────────────────────────
//
// One row per installed corpus that has an atlas on disk. Drives the
// /atlas index route. Mirrors `sovereign_tools::atlas_view::reader::
// AtlasCorpusSummary` — keep in sync.

export type AtomType =
  | "Entity"
  | "Event"
  | "State"
  | "Relation"
  | "Claim"
  | "Question"
  | "Configuration"
  | "ArgumentReconstruction";

export interface AtlasCorpusSummary {
  corpus_id: string;
  display_name: string;
  total_atoms: number;
  /** Per-type atom counts. Keys are a subset of `AtomType`; absent
   *  keys mean zero atoms of that type. */
  atom_counts: Partial<Record<AtomType, number>>;
  /** atoms.json mtime in unix seconds. Closest proxy for "last
   *  extracted at" until provenance metadata lands on the atom. */
  last_extracted_unix?: number;
  /** Logical UI category from the recipe's `[display]` block.
   *  Drives Atlas View rail grouping — corpora that share a category
   *  render under one header (e.g. `"conversation"` groups every
   *  conversation-source corpus together). `undefined` on legacy
   *  indexes pre-dating the field; the UI buckets those into
   *  "Other". */
  display_category?: string;
  /** Icon hint from the recipe's `[display]` block. Free-form
   *  string; the frontend maps known values onto its icon set. */
  display_icon?: string;
}

/** Server-side filter for `atlas_list_atoms`. All fields are
 *  independent — unset = "match anything". */
export interface AtomFilter {
  atom_type?: AtomType;
  /** Case-insensitive substring on display_name. */
  name_query?: string;
  /** Inclusive lower bound. Only Entity and Configuration carry a
   *  scalar score; other atom types are filtered out when set. */
  min_salience?: number;
}

export interface PageCursor {
  offset: number;
  limit: number;
}

// ─── Conversation tiered-retrieval Atlas surface ────────────────
// Spec: sovereign/docs/specs/CONV_TIERED_PORT.md §"Retrieval
// surface — A1/A2". Conv corpora don't write atoms.json; their
// tiered enrichment lives in the conv_skeletons / conv_raptor_nodes
// / conv_motifs SQLite sidecar tables. AtlasIndex calls BOTH
// atlasListCorpora and atlasListConvCorpora and merges the results.

/** One row in the desktop Atlas index for a conv corpus.
 *  Parallel to AtlasCorpusSummary but counts conversations and
 *  tracks per-state enrichment progress instead of atom-type
 *  buckets. */
export interface ConvCorpusSummary {
  corpus_id: string;
  display_name: string;
  /** Total conversations with at least one conv_skeletons row. */
  conv_count: number;
  /** State-bucketed counts. Keys are the ConvTieredState string
   *  variants: "Pending", "PartiallyReady", "MultiHopReady",
   *  "Ready", "Failed". Absent keys = zero. */
  state_counts: Partial<Record<string, number>>;
  last_updated_unix?: number;
  display_category?: string;
  display_icon?: string;
}

/** One row in AtlasConvCorpusView — a conversation as the
 *  atlas-level unit. */
export interface ConvSummary {
  conv_uuid: string;
  title: string;
  /** Verbatim from conv_skeletons.state. */
  state: string;
  chunk_count: number;
  /** Top entities across the conv's RAPTOR nodes by salience. Empty
   *  for Tiny synthetic convs. */
  top_entities: string[];
  updated_at: number;
  /** Tiny opt-2 path: single synthetic node with empty entities.
   *  UI suppresses entity affordances when true. */
  is_tiny: boolean;
}

export interface ConvListPage {
  conversations: ConvSummary[];
  total_matching: number;
  next_offset?: number;
}

/** One node in a conv's RAPTOR tree. */
export interface ConvRaptorNodeView {
  node_id: string;
  level: number;
  summary: string;
  primary_entities: string[];
  direct_member_chunk_ids: number[];
  evidence_chunk_count: number;
  cluster_coherence: number;
  /** Synthetic Tiny placeholder — UI suppresses entity row + shows
   *  a "no clusters extracted" affordance instead. */
  is_synthetic_tiny: boolean;
}

/** Full conv detail — title + state + full RAPTOR tree. The frontend
 *  picks flat (≤2 levels) or hierarchical (>2) rendering based on
 *  max_level. */
export interface ConvDetailView {
  corpus_id: string;
  conv_uuid: string;
  title: string;
  state: string;
  chunk_count: number;
  updated_at: number;
  raptor_nodes: ConvRaptorNodeView[];
  max_level: number;
}

/** One entity chip for the conversation chunk renderer (A2). */
export interface ConvEntityChip {
  name: string;
  /** Sum of cluster_coherence across nodes containing the entity. */
  salience: number;
  /** Number of distinct RAPTOR nodes mentioning the entity. */
  occurrence_count: number;
}

/** GliNER model availability + path. Drives the Settings → Imports
 *  download UI + AtlasIndex's "model not installed" warning. */
export interface GlinerModelStatus {
  installed: boolean;
  model_id: string;
  expected_path: string;
  size_estimate_mb: number;
}

/** One label's mention count inside an `EntityAggregateRow`.
 *  Splits the surface-form collapse (Person:"Swift" vs
 *  Organization:"SWIFT") so the drawer can show typed breakdown
 *  without merging homonyms. */
export interface EntityLabelCount {
  label: string;
  count: number;
}

/** One conversation that mentioned the queried entity. */
export interface EntityConvHit {
  conv_uuid: string;
  mention_count: number;
}

/** One entity co-appearing with the seed entity in the same chunks. */
export interface CoOccurringEntity {
  text: string;
  label: string;
  shared_chunk_count: number;
}

/** Aggregate view of one entity's footprint inside a corpus. Returned
 *  by `atlas_get_entity_aggregate`; powers the Atlas-view entity
 *  drawer. */
export interface EntityAggregateRow {
  corpus_id: string;
  /** Canonical display form (most-common surface variant in corpus). */
  text: string;
  labels: EntityLabelCount[];
  mention_count: number;
  conv_count: number;
  chunk_count: number;
  top_convs: EntityConvHit[];
  co_occurring: CoOccurringEntity[];
}

/** Per-corpus chunk-entity extraction progress. Mirrors the
 *  `chunk_entity_progress` SQLite row. State: "running" | "complete"
 *  | "incremental" | "failed" | "paused". The "incremental" state
 *  is the Phase B steady-state for live corpora (spec
 *  `sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md` §B) — Phase A
 *  finishes "complete", then the daemon's post-ingest hook flips it
 *  to "incremental" once the first delta-extract lands. */
export interface ChunkEntityProgressRow {
  corpus_id: string;
  chunks_processed: number;
  chunks_total: number;
  mentions_extracted: number;
  last_chunk_id?: number | null;
  started_at: number;
  updated_at: number;
  finished_at?: number | null;
  state: string;
  model_id?: string | null;
  threshold?: number | null;
  labels_json?: string | null;
  error_msg?: string | null;
}

export type CurationStatus = "generated";

/** Compact per-atom record returned by `atlas_list_atoms`. The full
 *  type-specific shape (premises[], evidence chunk previews, …)
 *  lives in `atlas_get_atom_detail` (Step 4). */
/** One node in the Atlas Map (an atom). `atom_type` is the backend
 *  `AtomType` serde string (e.g. "Entity", "Question",
 *  "ArgumentReconstruction"); `salience` present for Entity/Configuration. */
export interface AtlasNode {
  id: string;
  label: string;
  atom_type: string;
  salience?: number;
  degree: number;
}
/** One edge. `edge_type` is the `EdgeType` serde string; `crux` is the
 *  disagreement a "Tension" edge turns on. */
export interface AtlasEdge {
  source: string;
  target: string;
  edge_type: string;
  crux?: string;
}
export interface SubgraphCensus {
  atom_total: number;
  shown: number;
  tensions: number;
  questions: number;
  arguments: number;
}
/** Curated landscape subgraph for the Atlas Map view. */
export interface AtlasSubgraph {
  nodes: AtlasNode[];
  edges: AtlasEdge[];
  census: SubgraphCensus;
}

export interface AtomSummary {
  atom_id: string;
  stable_key: string;
  atom_type: AtomType;
  display_name: string;
  salience?: number;
  enrichment_depth: "structural" | "extracted" | "structural_classified";
  evidence_chunk_count: number;
  /** Phase 2 forward-compat — always "generated" in Phase 1. */
  curation_status: CurationStatus;
  /** Phase 2 forward-compat — always `false` in Phase 1. */
  overlay_supports: boolean;
  /** Unix seconds of the most recent (re)index of this atom's source
   *  document, when known. Present means the doc was refreshed after
   *  the bulk install (e.g. a newsworthy fetch) — the backend already
   *  sorts these to the top; the UI renders a "fresh" marker.
   *  Absent/null means baseline install-time content. */
  updated_at?: number | null;
}

export interface AtomListPage {
  items: AtomSummary[];
  total_matching: number;
  next_offset?: number;
}

// ─── Atom Detail (Phase 1 Step 4) ────────────────────────────
//
// Full inspector record. Mirrors `sovereign_tools::atlas_view::
// atom_detail::AtomDetail` — keep in sync. The `atom` field is the
// raw AtomEnvelope tagged shape, same as on-disk atoms.json: one of
// 8 variants discriminated by `atom_type`.

export interface ChunkRefData {
  chunk_id: string;
  passage_preview?: string;
}

export interface SectionRangeData {
  start: string;
  end: string;
}

export interface SectionPositionData {
  section_id: string;
  paragraph_index?: number;
}

/** Loose typing of per-variant payloads. Each type-body Svelte
 *  component (`EntityBody`, `ClaimBody`, …) narrows the shape it
 *  needs at render time. Avoids modeling all 8 corpus-engine
 *  structs in TS just to render fields — Phase 1 pragma. */
export type AtomEnvelope =
  | { atom_type: "Entity"; data: EntityData }
  | { atom_type: "Event"; data: EventData }
  | { atom_type: "State"; data: StateData }
  | { atom_type: "Relation"; data: RelationData }
  | { atom_type: "Claim"; data: ClaimData }
  | { atom_type: "Question"; data: QuestionData }
  | { atom_type: "Configuration"; data: ConfigurationData }
  | { atom_type: "ArgumentReconstruction"; data: ArgumentReconstructionData };

// NOTE: Vec<>/Option<> fields on the corpus-engine atom structs use
// `#[serde(default, skip_serializing_if = "...")]`, so empty / None
// values are **omitted from the wire** rather than serialized as
// `[]` / `null`. The TS types mark those fields optional so render
// code uses `?? []` or `?.length` and doesn't crash on undefined.

export interface EntityData {
  id: string;
  canonical_name: string;
  aliases?: string[];
  entity_type: string;
  first_appearance: ChunkRefData;
  description: string;
  defining_quote?: string;
  salience: number;
  enrichment_depth: string;
  affiliation?: string;
  role?: string;
  participants?: string[];
}

export interface EventData {
  id: string;
  description: string;
  event_type: string;
  participants?: string[];
  evidence?: ChunkRefData[];
  section_position: SectionPositionData;
  causal_antecedents?: string[];
  enrichment_depth: string;
}

export interface StateData {
  id: string;
  entity_id: string;
  label: string;
  state_type: string;
  evidence?: ChunkRefData[];
  section_range: SectionRangeData;
  confidence?: number;
  enrichment_depth: string;
}

export interface RelationData {
  id: string;
  label: string;
  participants: string[];
  relation_type: string;
  evidence?: ChunkRefData[];
  section_range: SectionRangeData;
  enrichment_depth: string;
}

export interface ClaimData {
  id: string;
  content: string;
  discourse_act: string;
  epistemic_status: string;
  scope: string;
  evidence?: ChunkRefData[];
  quotable_excerpt?: string;
  attributed_to?: string;
  confidence?: number;
  enrichment_depth: string;
}

export interface QuestionData {
  id: string;
  content: string;
  question_type: string;
  addressed_by?: string[];
  raised_at?: ChunkRefData[];
  /** Tagged union — `kind` is `"resolved" | "contested" | "open" | "dissolved"`. */
  resolution_status: { kind: string; claim_id?: string; claim_ids?: string[] };
  enrichment_depth: string;
}

export interface ConfigurationData {
  id: string;
  label: string;
  description: string;
  constituent_atoms: string[];
  evidence?: ChunkRefData[];
  confidence: number;
  interpretive_note: string;
  enrichment_depth: string;
}

export interface ObjectionData {
  name: string;
  /** Legacy atoms.json files (pre-2026) carried bare strings for
   *  objections; the deserialiser fills `content: ""` in that case. */
  content?: string;
}

export interface ArgumentReconstructionData {
  id: string;
  name: string;
  proponent?: string;
  premises: string[];
  conclusion: string;
  objections?: ObjectionData[];
  evidence?: ChunkRefData[];
  section_position: SectionPositionData;
  enrichment_depth: string;
}

export interface EvidenceExcerpt {
  section_id: string;
  /** Numeric chunk id from `index.resolve_sections_to_chunks`,
   *  populated by `atlas_get_atom_detail` at the Tauri boundary.
   *  Present → the evidence row is clickable and deep-links into
   *  ReadingSurface. Absent → resolution failed (missing chunk,
   *  index not loaded), row stays read-only. */
  chunk_id?: number;
  passage_preview?: string;
}

export interface RelatedAtom {
  atom_id: string;
  atom_type: AtomType;
  display_name: string;
  edge_type: string;
  role: string;
  confidence: number;
}

export interface CrossCorpusLink {
  peer_corpus_id: string;
  peer_atom_id: string;
  peer_canonical_name: string;
  edge_type: string;
  signal: string;
  confidence: number;
}

/** Display label for an atom referenced by the focal atom's body
 *  fields (`Claim.attributed_to`, `State.entity_id`, etc.). The
 *  desktop uses this to render `<AtomLink>` chips instead of opaque
 *  `entity-0042` mono-text. Dangling references (atom_id not found
 *  in atoms.json) are omitted from the map entirely. */
export interface ReferencedAtom {
  display_name: string;
  atom_type: AtomType;
}

export interface AtomDetail {
  corpus_id: string;
  atom_id: string;
  stable_key: string;
  atom_type: AtomType;
  display_name: string;
  salience?: number;
  atom: AtomEnvelope;
  evidence_excerpts: EvidenceExcerpt[];
  related: RelatedAtom[];
  cross_corpus: CrossCorpusLink[];
  /** Per-`atom_id` labels for every reference inside the focal
   *  atom's payload. Keys are atom_ids; values are the display
   *  label + type. Use via the `atomLinkResolver` Svelte context. */
  referenced_atoms: Record<string, ReferencedAtom>;
  extraction_run?: string;
  curation_status: CurationStatus;
  overlay_supports: boolean;
}

// ─── Peer-assisted ingest ("Blanket") ──────────────────────────────
// Mirrors the daemon DTOs in commonwealth-api routes_internal
// (corpus_collaborate / corpus_grant / corpus_queue).

/** Why a peer can or can't help with a peer-assisted ingest. Machine token
 *  kept in lockstep with the backend candidate filter so copy stays honest. */
export type AssistIneligibleReason =
  | "ok"
  | "offline"
  | "no_embed_model"
  | "embed_model_mismatch";

export interface AssistEligiblePeer {
  node_id: string; // full hex
  name: string;
  online: boolean;
  eligible: boolean;
  reason: AssistIneligibleReason;
}

export interface AssistEligiblePeersResponse {
  peers: AssistEligiblePeer[];
  /** Whether this corpus may be peer-assisted at all (`[corpus] grantable`). */
  grantable: boolean;
}

export interface AssistStartResult {
  corpus_id: string;
  /** Opaque handoff id — round-trip back to meshAssistStatus unchanged. */
  handoff_id: unknown;
  grant_expires_at_ms: number;
  peer_count: number;
}

export interface AssistPeerProgress {
  node_id: string;
  leased: number;
  completed: number;
  failed: number;
}

export interface AssistGrant {
  expires_at_ms: number;
  revoked: boolean;
  allowed_peers: string[];
}

export interface AssistVerification {
  sampled: number;
  passed: number;
  min_cosine: number;
  /** [sample_index, cosine] for each chunk that missed tolerance. */
  failures: [number, number][];
}

export interface CollaborateStatus {
  handoff_id: string;
  corpus_id: string;
  phase: string;
  total_units: number;
  complete: number;
  failed: number;
  leased: number;
  queued: number;
  per_peer: AssistPeerProgress[];
  ephemeral: boolean;
  grant: AssistGrant | null;
  verification: AssistVerification | null;
}

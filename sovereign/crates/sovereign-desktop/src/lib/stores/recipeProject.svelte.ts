// Runed singleton for the **Recipe Author Workspace** (M2).
//
// Owns three pieces of state:
// - `projects`        — the sidebar list (newest first)
// - `selectedFeatureId` — which project is open in the dashboard
// - `dashboard`       — the coarse `recipe_author_dashboard_state`
//                       struct for the selected project
//
// While a project is selected the store polls
// `recipe_author_dashboard_state` every `POLL_INTERVAL_MS` so the
// cards stay live. The plan calls 2 s polling out as v1 — SSE is
// deferred. Polling stops when no project is selected (or when the
// workspace deactivates) so we don't burn CPU in the chat workspace.
//
// The store also drives the workspace skill toggle: `activate()` on
// mount flips `recipe-author` into `active_skills` so chat
// conversations started here are tagged with that skill.
//
// Errors during refresh are surfaced via `lastError` rather than
// thrown — a failed poll shouldn't crash the workspace; the dashboard
// reads the prior state until the next refresh succeeds.

import {
  recipeAuthorDashboardState,
  recipeAuthorListProjects,
  recipeAuthorNewProject,
  recipeAuthorRestoreCheckpoint,
} from "../api";
import type {
  RecipeAuthorDashboardState,
  RecipeProjectListEntry,
  RestoreCheckpointOutcome,
} from "../types";

const POLL_INTERVAL_MS = 2000;

let _projects: RecipeProjectListEntry[] = $state([]);
let _selectedFeatureId: string | null = $state(null);
let _dashboard: RecipeAuthorDashboardState | null = $state(null);
let _lastError: string | null = $state(null);
let _loading = $state(false);

let _pollHandle: ReturnType<typeof setInterval> | null = null;

async function refreshProjects(): Promise<void> {
  try {
    _projects = await recipeAuthorListProjects();
    _lastError = null;
  } catch (e) {
    _lastError = String(e);
    console.warn("recipeProject: list failed:", e);
  }
}

async function refreshDashboard(): Promise<void> {
  if (!_selectedFeatureId) {
    _dashboard = null;
    return;
  }
  try {
    _dashboard = await recipeAuthorDashboardState(_selectedFeatureId);
    _lastError = null;
  } catch (e) {
    _lastError = String(e);
    console.warn("recipeProject: dashboard refresh failed:", e);
  }
}

function startPolling(): void {
  if (_pollHandle) return;
  _pollHandle = setInterval(() => {
    void refreshDashboard();
  }, POLL_INTERVAL_MS);
}

function stopPolling(): void {
  if (_pollHandle) {
    clearInterval(_pollHandle);
    _pollHandle = null;
  }
}

export const recipeProjectStore = {
  /** Sidebar list — newest first. */
  get projects() {
    return _projects;
  },
  /** feature_id of the currently-selected project, or null when no
   *  project is open. */
  get selectedFeatureId() {
    return _selectedFeatureId;
  },
  /** Coarse dashboard state for the selected project. `null` when
   *  none is selected or the first poll hasn't returned yet. */
  get dashboard() {
    return _dashboard;
  },
  /** Most recent backend error (list/refresh/restore/etc), or null
   *  when the last call succeeded. Surfaced as a workspace-level
   *  banner. */
  get lastError() {
    return _lastError;
  },
  /** True while an explicit user-triggered call (refresh, new,
   *  restore) is in flight — drives spinners on action buttons. */
  get loading() {
    return _loading;
  },

  /** Mount entry point: refreshes the project list and (if a
   *  project is selected) kicks off polling. Idempotent — safe to
   *  call repeatedly on workspace re-mounts.
   *
   *  Pre-2026-05-24 this also toggled the recipe-author skill on
   *  via `recipeAuthorSetWorkspaceActive` (which triggered a
   *  ~15s `rebuild_runtime` pass). Routing is now driven by the
   *  conversation's surface tag set at create-time
   *  (`RecipeChatSurface.SURFACE_SKILL_ID`), so the workspace mount
   *  no longer touches global skill state. */
  async activate(): Promise<void> {
    _loading = true;
    try {
      await refreshProjects();
      await refreshDashboard();
      if (_selectedFeatureId) startPolling();
    } finally {
      _loading = false;
    }
  },

  /** Tear-down. Stops polling. Skill toggle removed (see
   *  `activate`). */
  async deactivate(): Promise<void> {
    stopPolling();
  },

  /** Switch which project is open. Triggers an immediate dashboard
   *  refresh and (re)starts polling. Pass `null` to clear the
   *  selection (back to the empty workspace state). */
  async select(featureId: string | null): Promise<void> {
    _selectedFeatureId = featureId;
    _dashboard = null;
    if (featureId) {
      await refreshDashboard();
      startPolling();
    } else {
      stopPolling();
    }
  },

  /** Manual list refresh (e.g. after creating a new project). */
  async refreshList(): Promise<void> {
    _loading = true;
    try {
      await refreshProjects();
    } finally {
      _loading = false;
    }
  },

  /** Force a dashboard refresh without waiting for the next poll
   *  tick. Used right after a mutation (new project, restore) so
   *  the UI reflects the change immediately. */
  async refreshDashboard(): Promise<void> {
    await refreshDashboard();
  },

  /** Create a project from a title + charter. Selects the new
   *  project on success so the dashboard renders immediately. */
  async createProject(
    title: string,
    charterMd: string,
  ): Promise<RecipeProjectListEntry> {
    _loading = true;
    try {
      const entry = await recipeAuthorNewProject(title, charterMd);
      await refreshProjects();
      await this.select(entry.feature_id);
      return entry;
    } finally {
      _loading = false;
    }
  },

  /** Restore the selected project to the given checkpoint. The
   *  backend lays down a restore-anchor checkpoint and (when the
   *  project has a recipe id) overwrites the live recipe.toml.
   *  Refreshes the dashboard on success. */
  async restoreCheckpoint(
    checkpointId: string,
  ): Promise<RestoreCheckpointOutcome | null> {
    if (!_selectedFeatureId) return null;
    _loading = true;
    try {
      const outcome = await recipeAuthorRestoreCheckpoint(
        _selectedFeatureId,
        checkpointId,
      );
      await refreshDashboard();
      return outcome;
    } finally {
      _loading = false;
    }
  },
};

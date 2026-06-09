// SPDX-License-Identifier: AGPL-3.0-or-later
// Inner Work session store.
//
// Phase 2 holds three pieces of cross-component state:
//
//   1. `thresholdShown` — once-per-window-session flag for the 800ms
//      threshold transition. Resets only on page reload, so re-entering
//      the surface during a session lands on the page directly.
//
//   2. Draft text (localStorage-backed, keyed by ISO date). The
//      in-progress text the user is typing but hasn't yet summoned the
//      witness for. Cleared once it's committed via `Cmd+Return` (the
//      committed turn lives in the conversation). Persisted so a window
//      close + reopen doesn't lose work.
//
//   3. Date → conversation_id map. Today's inner-work entry is backed
//      by a chat conversation pinned to the inner-work skill. The
//      mapping lives client-side because Phase 2 doesn't introduce a
//      backend tag for "this is an inner-work conversation"; instead,
//      we remember the id locally and resume it on re-open.

const DRAFT_PREFIX = "sovereign:inner_work:";
const CONV_PREFIX = "sovereign:inner_work:conv:";

let _thresholdShown = $state(false);
let _hintsShown = $state(false);

function todayIsoDate(now: Date = new Date()): string {
  // Local-time YYYY-MM-DD. Using the user's local date avoids the
  // "I wrote at 11pm and now my entry is on tomorrow" UTC pitfall.
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function loadDraft(date: string): string {
  try {
    return localStorage.getItem(DRAFT_PREFIX + date) ?? "";
  } catch {
    return "";
  }
}

function saveDraft(date: string, text: string): void {
  try {
    if (text.length === 0) {
      localStorage.removeItem(DRAFT_PREFIX + date);
    } else {
      localStorage.setItem(DRAFT_PREFIX + date, text);
    }
  } catch {
    // Quota errors etc. — silently drop.
  }
}

function getConversationIdFor(date: string): string | null {
  try {
    return localStorage.getItem(CONV_PREFIX + date);
  } catch {
    return null;
  }
}

function setConversationIdFor(date: string, id: string): void {
  try {
    localStorage.setItem(CONV_PREFIX + date, id);
  } catch {
    // Quota errors etc. — drop. Worst case the user gets a fresh
    // conversation on the next visit, with the prior turns still
    // accessible from the main conversation list.
  }
}

function clearConversationIdFor(date: string): void {
  try {
    localStorage.removeItem(CONV_PREFIX + date);
  } catch {
    // ignore
  }
}

export interface InnerWorkEntryIndex {
  dateIso: string;
  conversationId: string;
}

/// Enumerate every (date → conversation_id) mapping this client has
/// recorded for inner-work entries. The source of truth is the
/// `CONV_PREFIX` keys in localStorage — every successful first-summon
/// of the day writes one. Sorted descending by date so the caller can
/// render most-recent-first without re-sorting.
///
/// Why this is the index, not a `list_conversations` title filter:
/// the conversation rename to `Inner Work — <dateline>` is best-
/// effort; a rename failure leaves the entry untitled and would slip
/// past a title-prefix filter. localStorage is the device's own log
/// of "I summoned the witness on this date," which is exactly the
/// question the past-entries drawer asks.
function listEntryIndex(): InnerWorkEntryIndex[] {
  const out: InnerWorkEntryIndex[] = [];
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || !key.startsWith(CONV_PREFIX)) continue;
      const dateIso = key.slice(CONV_PREFIX.length);
      const conversationId = localStorage.getItem(key);
      if (!conversationId) continue;
      out.push({ dateIso, conversationId });
    }
  } catch {
    return out;
  }
  out.sort((a, b) => (a.dateIso < b.dateIso ? 1 : a.dateIso > b.dateIso ? -1 : 0));
  return out;
}

export const innerWorkSession = {
  get thresholdShown(): boolean {
    return _thresholdShown;
  },

  /// Mark the threshold as already-played for this window session.
  /// Called by the surface after the 800ms transition completes.
  markThresholdShown(): void {
    _thresholdShown = true;
  },

  get hintsShown(): boolean {
    return _hintsShown;
  },

  /// Mark the welcome hints as already-played for this window session.
  /// Window-scoped (not localStorage) so reload re-plays them — long-
  /// term users who reload don't get treated as veterans, and the
  /// hints are gentle enough that re-seeing them isn't intrusive.
  markHintsShown(): void {
    _hintsShown = true;
  },

  /// Today's date in YYYY-MM-DD form, computed at call time so a
  /// long-lived window crossing midnight reflects the new day.
  todayIsoDate,

  loadDraft,
  saveDraft,
  getConversationIdFor,
  setConversationIdFor,
  clearConversationIdFor,
  listEntryIndex,
};

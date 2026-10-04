// SPDX-License-Identifier: AGPL-3.0-or-later
// Minimal app-wide toast store.
//
// Scope: ephemeral, non-blocking notifications that need to reach the
// user even when they've navigated away from the component that
// triggered them. The first real consumer is the atlas-complete
// "Ready to ask about X, Y, Z — what connections can we make?"
// message — if the user drops to chat while the sample atlas
// finishes in the background, the toast is how they learn the
// questions are ready.
//
// Design choices:
//   - One toast at a time. A second `notify*` call replaces the
//     current one rather than stacking; keeps the UI calm.
//   - Toasts have a primary action button that can carry a
//     payload-bound handler (e.g., "Ask a question" with a
//     pre-selected StarterQuestion).
//   - Auto-dismiss timer owned by the store so component unmounts
//     don't leak timers; callers can clear() early if needed.
//   - Rendered by a single `<ToastHost>` in App.svelte.

import type { StarterQuestion } from "../types";
import { cleanExcerptTitle } from "../onboarding/excerpt_helpers";
import { normalizeError } from "../errors";

/// Shape of one active toast. Kept minimal — if more variants land
/// we'll switch to a tagged union.
export interface Toast {
  /// Unique id for keying + replacement tracking.
  id: string;
  /// Primary line, e.g. "Ready to ask about Alpha, Beta, and 3 more".
  title: string;
  /// Optional second line rendered smaller beneath the title.
  body?: string;
  /// Optional action button label. If present, `onAction` should be
  /// too; clicking fires `onAction()` and dismisses the toast.
  actionLabel?: string;
  /// Fired on action click. Safe to await inside.
  onAction?: () => void;
}

const DEFAULT_TTL_MS = 8000;

/// Surface a (possibly structured) command failure as a toast (§2D-3).
/// Any rejection from `invokeChecked` is already a `DesktopError`;
/// legacy/string errors are normalised first. The `suggested_action`
/// becomes the toast body so the user sees a next step, not just a
/// failure line.
export function toastError(e: unknown): void {
  const err = normalizeError(e);
  toastStore.notify({
    title: err.message,
    body: err.suggested_action || undefined,
  });
}

let _current: Toast | null = $state(null);
let _timer: ReturnType<typeof setTimeout> | null = null;

function clearTimer() {
  if (_timer) {
    clearTimeout(_timer);
    _timer = null;
  }
}

function scheduleDismiss(id: string, ttl: number) {
  clearTimer();
  _timer = setTimeout(() => {
    if (_current?.id === id) {
      _current = null;
      _timer = null;
    }
  }, ttl);
}

export const toastStore = {
  get current(): Toast | null {
    return _current;
  },

  /// Raise a toast. Replaces any currently-visible toast.
  notify(toast: Omit<Toast, "id">, ttlMs: number = DEFAULT_TTL_MS): void {
    const id = `toast-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    _current = { ...toast, id };
    scheduleDismiss(id, ttlMs);
  },

  /// Dismiss the current toast if any. Safe to call when empty.
  clear(): void {
    clearTimer();
    _current = null;
  },

  /// Internal: used by ToastHost when the user clicks the action or
  /// close button. Resets `_current` synchronously so multiple
  /// callers can't race on the same toast.
  _consume(id: string): Toast | null {
    if (_current?.id !== id) return null;
    const t = _current;
    clearTimer();
    _current = null;
    return t;
  },
};

/// High-level helper: called by FolderDropFlow when a sample atlas
/// build finishes. Shapes the toast copy around document titles.
export function notifyReadyToAsk(input: {
  corpusId: string;
  titles: string[];
  total: number;
  firstStarter: StarterQuestion | null;
  /// Fired when the user clicks "Ask a question" on the toast. The
  /// host (App.svelte) routes this into the chat-seed handoff.
  onAsk: (question: StarterQuestion) => void;
}): void {
  const { titles, total, firstStarter, onAsk } = input;
  const cleaned = titles.map(cleanExcerptTitle).filter((t) => t);
  const title =
    cleaned.length === 0
      ? "Atlas ready"
      : `Ready to ask about ${formatTitleList(cleaned)}`;
  const coverage =
    total > titles.length
      ? ` (${titles.length} of ${total} documents — rest stay searchable)`
      : "";
  const body = `What connections can we make?${coverage}`;
  // Only expose the "Ask" action when there's a starter question to
  // seed. Otherwise the user can navigate to chat themselves; the
  // empty-state chips will refresh automatically.
  const actionLabel = firstStarter ? "Ask a question" : undefined;
  const onAction = firstStarter ? () => onAsk(firstStarter) : undefined;
  toastStore.notify({ title, body, actionLabel, onAction });
}

function formatTitleList(titles: string[]): string {
  if (titles.length === 1) return titles[0];
  if (titles.length === 2) return `${titles[0]} and ${titles[1]}`;
  if (titles.length === 3)
    return `${titles[0]}, ${titles[1]}, and ${titles[2]}`;
  return `${titles[0]}, ${titles[1]}, and ${titles.length - 2} more`;
}

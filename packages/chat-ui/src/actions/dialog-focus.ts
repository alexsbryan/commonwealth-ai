// SPDX-License-Identifier: AGPL-3.0-or-later
// `use:dialogFocus` — the keyboard + focus contract for a modal dialog,
// in one place. Shared by both apps (desktop + mobile) because every
// dialog needs the SAME three behaviours and, before this, each one
// hand-rolled a subset:
//
//   1. Focus moves INTO the dialog on open (first tabbable, else the
//      dialog node itself).
//   2. Tab / Shift-Tab CYCLE within the dialog — focus can't escape to
//      the page behind the scrim.
//   3. On close, focus RESTORES to the element that was focused before
//      the dialog opened. This was the gap across every existing dialog:
//      a keyboard or screen-reader user who opened a dialog was dumped
//      at the top of the document when it closed, losing their place.
//
// DOM-only: the sole import is a type, so this file resolves with zero
// npm dependencies and `tsc` type-checks it cleanly from the shared
// package directory (unlike the xstate/marked modules the barrel
// intentionally does NOT share).
//
// Scope boundary: the action owns KEYBOARD + FOCUS only. Backdrop
// click-to-close stays the host's `onclick` on the scrim, so dialogs
// keep their existing pointer behaviour untouched.
import type { Action } from "svelte/action";

export interface DialogFocusParams {
  /** Invoked on Escape. Hosts pass their close/cancel handler here. */
  onEscape?: () => void;
  /** Override the initial focus target. Defaults to the first tabbable
   *  descendant, falling back to the dialog node itself. */
  initial?: HTMLElement | null;
}

// Elements that take keyboard focus in normal flow. `[tabindex="-1"]`
// is excluded — it's programmatically focusable but not in the Tab
// order, which is exactly the dialog node's own role.
const TABBABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

export const dialogFocus: Action<
  HTMLElement,
  DialogFocusParams | undefined
> = (node, params) => {
  // Capture who had focus BEFORE the dialog opened. The action runs
  // after the node is inserted but synchronously within the same task,
  // so `document.activeElement` is still the opener (the focus move
  // below happens in a microtask). Restored in destroy().
  const previouslyFocused = document.activeElement as HTMLElement | null;

  function tabbables(): HTMLElement[] {
    return Array.from(
      node.querySelectorAll<HTMLElement>(TABBABLE_SELECTOR),
      // offsetParent === null filters display:none subtrees, so Tab
      // skips controls hidden behind an {#if} branch (e.g. a dialog's
      // loading vs. ready states).
    ).filter((el) => el.offsetParent !== null || el === document.activeElement);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      // Stop the Escape from also bubbling to a parent handler (some
      // hosts previously had their own backdrop Escape that we replace).
      event.stopPropagation();
      params?.onEscape?.();
      return;
    }
    if (event.key !== "Tab") return;

    const items = tabbables();
    if (items.length === 0) {
      // Nothing tabbable yet (e.g. a "Loading…" state) — keep focus
      // pinned to the dialog so Tab can't escape to the page.
      event.preventDefault();
      node.focus();
      return;
    }

    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && (active === first || !node.contains(active))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  // Move focus in on mount. Deferred a microtask so any {#if}/{#await}
  // content inside the dialog has rendered before we pick a target.
  queueMicrotask(() => {
    const target = params?.initial ?? tabbables()[0] ?? node;
    target.focus();
  });

  node.addEventListener("keydown", onKeydown);

  return {
    update(next: DialogFocusParams | undefined) {
      params = next;
    },
    destroy() {
      node.removeEventListener("keydown", onKeydown);
      // The restore. Guarded: the opener may have been removed from the
      // DOM while the dialog was open, in which case .focus() is a no-op.
      previouslyFocused?.focus?.();
    },
  };
};

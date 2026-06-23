// SPDX-License-Identifier: AGPL-3.0-or-later
// Screen-reader completion wording, single-sourced across the desktop
// and mobile chat surfaces.
//
// Why a shared helper and not an inline string per app: the *trigger*
// (the streaming → idle edge of each app's own chat FSM) is necessarily
// per-app, because each app keeps its own copy of `chat.machine.ts`
// (the xstate FSM can't be shared cleanly through this package — see the
// barrel comment in ./index.ts). But the *wording* is the part that
// drifts, so it lives here once. Pure and dependency-free, so `tsc`
// type-checks it cleanly from this directory (no npm imports to resolve).
//
// The contract (the a11y win this enables): announce ONCE when an
// assistant turn finishes — never per token. Putting `aria-live` on the
// streaming text re-announces the whole growing answer on every chunk,
// which is unusable with a screen reader. Callers render this string
// into a separate, visually-hidden polite live region only on the
// completion edge.

export interface CompletionAnnouncementOpts {
  /** The turn ended in an error tail rather than a clean completion. */
  errored?: boolean;
}

/** The polite-live-region sentence for a finished assistant turn. */
export function completionAnnouncement(
  opts: CompletionAnnouncementOpts = {},
): string {
  return opts.errored
    ? "Sovereign hit an error responding."
    : "Sovereign finished responding.";
}

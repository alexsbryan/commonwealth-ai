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
    ? "svrnmesh hit an error responding."
    : "svrnmesh finished responding.";
}

/**
 * The completion TRIGGER, shared for the same reason the wording is.
 *
 * Feed it each render's "is this turn streaming" flag; it emits exactly once,
 * on the streaming -> idle FALLING edge, and never while streaming holds. That
 * is the whole a11y contract: a live region fed on every chunk re-announces
 * the entire growing answer per token, which is unusable with a screen reader.
 *
 * The edge lived inline in the desktop's ChatView as a `wasStreaming` local.
 * A second surface would have hand-rolled its own copy — and the header above
 * already says the *wording* was shared precisely because that is what drifts.
 * The edge drifts the same way and is one boolean wide, so it belongs here
 * with it. It takes a flag, not an FSM, so nothing about either app's own
 * `chat.machine.ts` leaks into this package.
 *
 * @param emit called with the sentence to render into the polite live region.
 * @returns `observe(streaming, opts)` -> whether it announced on this tick,
 *          so a caller can clear per-turn state (an error flag) only when a
 *          turn actually ended.
 */
export function completionAnnouncer(emit: (text: string) => void): {
  observe: (streaming: boolean, opts?: CompletionAnnouncementOpts) => boolean;
} {
  let wasStreaming = false;
  return {
    observe(streaming: boolean, opts: CompletionAnnouncementOpts = {}): boolean {
      const falling = wasStreaming && !streaming;
      wasStreaming = streaming;
      if (falling) emit(completionAnnouncement(opts));
      return falling;
    },
  };
}

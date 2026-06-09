// Pure text/date helpers extracted from InnerWorkSurface.svelte (§3.3
// component decomposition): witness-error humanization, the echo
// similarity tokenizer, and the two date formatters. No runes, no IO —
// unit-tested; the component imports them.

/**
 * Render a raw inference-layer error message as a short line the writer
 * can act on without leaving the surface. The witness slot is one italic
 * line under the user's paragraph — long technical strings would dominate
 * the column.
 *
 * Match shape: the context-overflow case is the dominant cause of silent
 * witness drop today (entries grow per-turn as memories accumulate), so it
 * gets a tailored "start a new entry" hint. Everything else collapses to a
 * generic non-blaming line; the raw message lives in `console.warn` for
 * triage.
 */
export function humanizeWitnessError(raw: string): string {
  if (raw.includes("Prompt too long")) {
    return "This entry has grown past the witness's window — start a new entry to continue.";
  }
  return "The witness couldn't respond. Try again, or keep writing.";
}

/**
 * Lowercase word-set for echo similarity. Words longer than 3 chars filter
 * out most function words ("the", "and", "for") without a stopword list.
 */
export function tokenize(s: string): Set<string> {
  const out = new Set<string>();
  for (const w of s.toLowerCase().split(/\W+/)) {
    if (w.length > 3) out.add(w);
  }
  return out;
}

/**
 * Human relative date for an epoch-seconds timestamp: "earlier today",
 * "yesterday", "N days ago" (< 7), else a long month/day date. `now` is
 * injectable so the branches are deterministically testable.
 */
export function formatRelativeDate(
  epochSeconds: number,
  now: Date = new Date(),
): string {
  const then = new Date(epochSeconds * 1000);
  const diffMs = now.getTime() - then.getTime();
  const diffDays = Math.floor(diffMs / (1000 * 60 * 60 * 24));
  if (diffDays <= 0) return "earlier today";
  if (diffDays === 1) return "yesterday";
  if (diffDays < 7) return `${diffDays} days ago`;
  return then.toLocaleDateString(undefined, {
    month: "long",
    day: "numeric",
  });
}

/** Full weekday/month/day dateline from a `YYYY-MM-DD` string (local time). */
export function formatDateline(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  const dt = new Date(y, m - 1, d);
  return dt.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
}

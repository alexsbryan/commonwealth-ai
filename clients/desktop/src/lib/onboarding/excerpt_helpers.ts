// SPDX-License-Identifier: AGPL-3.0-or-later
// Presentation helpers for `IngestStats.excerpt_chunks`.
//
// The Rust side picks 3 excerpts using length + diversity +
// (recently) a content-quality bias. This module does the
// final-mile display cleanup and synthesises starter-question
// chips from the excerpts when no atlas-mined ones are ready yet.
//
// Everything here is pure and synchronous — no Tauri calls, no LLM.
// Excerpts arrive in the completion payload; we just make them
// presentable and give the user a click-to-ask affordance.

import type { ExcerptChunk, StarterQuestion } from "../types";

/// Clean a source title like "11. Erwin Schrodinger What is Life
/// 1944" into something that reads as a book, not a filename.
///
/// Rules, applied in order:
///   1. Strip leading `NN.` / `NN-` / `NN_` numeric prefix.
///   2. Strip trailing year token `(\d{4})?` / ` \d{4}$`.
///   3. Strip trailing file extensions that may have survived the
///      Rust-side humanise pass ("What is Life pdf").
///   4. Collapse whitespace.
///
/// Conservative — if the title doesn't match a pattern we leave it
/// alone rather than mangle a well-named doc.
export function cleanExcerptTitle(raw: string): string {
  let t = (raw ?? "").trim();
  if (!t) return "Untitled";
  // Leading numeric prefix: "11. ", "11 - ", "11_".
  t = t.replace(/^\d{1,4}[.\-_]\s*/, "");
  // Trailing year (1700-2099), loose: " 1944", "(1944)", "[1944]".
  t = t.replace(/\s*[[(]?(17|18|19|20)\d{2}[\])]?\s*$/, "");
  // Surviving file extensions.
  t = t.replace(/\s+(pdf|txt|md|epub|docx)\s*$/i, "");
  return t.replace(/\s+/g, " ").trim() || raw.trim();
}

/// Trim an excerpt body to a displayable snippet.
///
/// When `title` is provided, strips any leading echo of it first:
/// the chunker prepends the document title to each chunk body for
/// retrieval context — great for recall, noisy on screen. The
/// pre-fix version rendered chunks like
/// `"11. Erwin Schrodinger What is Life 1944 novel you are reading
/// is…"` where the filename-ish title leaked into the displayed
/// sentence.
///
/// Cleanup order:
///   1. Collapse whitespace.
///   2. Strip a leading title-echo (raw or cleaned shape).
///   3. If the remaining text starts mid-sentence (lowercase), try
///      to seek forward to the next sentence start — but only if
///      that start lives in the first ~30 % of the cap. Past that,
///      prepend `…` so the reader knows the quote starts mid-thought
///      without throwing away a load-bearing opening clause.
///   4. Length cap at a word/sentence boundary; append `…` if cut.
export function cleanExcerptBody(
  raw: string,
  title: string | null = null,
  maxLen: number = 260,
): string {
  let t = (raw ?? "").replace(/\s+/g, " ").trim();
  if (!t) return "";

  t = stripTitlePrefix(t, title);

  // Seek forward to a clean sentence start when one is reachable
  // within the opening window; otherwise flag the mid-sentence
  // opener with a leading ellipsis.
  const sentenceStartWindow = Math.floor(maxLen * 0.3);
  if (/^[a-z]/.test(t)) {
    const match = t.match(/[.!?]\s+[A-Z]/);
    if (
      match &&
      typeof match.index === "number" &&
      match.index < sentenceStartWindow
    ) {
      t = t.slice(match.index + match[0].length - 1).trim();
    } else {
      t = `…${t}`;
    }
  }

  if (t.length <= maxLen) {
    // Still add an ellipsis if the source chunk didn't end on a
    // terminator — signals "there's more in the doc".
    return /[.!?…]$/.test(t) ? t : `${t}…`;
  }

  const cut = t.slice(0, maxLen);
  // Prefer a sentence break if one exists in the last third.
  const lastPeriod = cut.lastIndexOf(". ");
  if (lastPeriod > maxLen * 0.65) {
    return t.slice(0, lastPeriod + 1);
  }
  const lastSpace = cut.lastIndexOf(" ");
  const trimmed = (lastSpace > 0 ? cut.slice(0, lastSpace) : cut).trim();
  return `${trimmed}…`;
}

/// Remove a leading title echo from a chunk body. Matches both the
/// raw title (what the chunker actually prepended) and the cleaned
/// display title — either might appear depending on when cleanup
/// was run.
function stripTitlePrefix(text: string, title: string | null): string {
  if (!title) return text;
  const candidates = Array.from(
    new Set([title.trim(), cleanExcerptTitle(title).trim()]),
  );
  for (const cand of candidates) {
    if (!cand || cand.length < 3) continue;
    if (text.length <= cand.length + 1) continue;
    const head = text.slice(0, cand.length).toLowerCase();
    if (head !== cand.toLowerCase()) continue;
    // Only strip when the title ends on a boundary in the chunk —
    // otherwise the title is a substring of a real opening word,
    // not a prepended echo.
    const next = text.charAt(cand.length);
    if (!/[\s,.—–\-:;]/.test(next)) continue;
    return text.slice(cand.length).replace(/^[\s,.—–\-:;]+/, "").trim();
  }
  return text;
}

/// Synthesise click-to-ask starter questions from a set of
/// excerpts. Used when the atlas isn't ready yet (or failed) —
/// gives the user a concrete payoff immediately after ingest.
///
/// Deliberately template-driven: the questions are shaped so the
/// model gets the *document title* as context. Plus one
/// cross-document question when we have 2+ sources, since "what
/// connects these?" is exactly the capability the atlas is
/// supposed to unlock — asking it up front is a good Rorschach
/// for whether the corpus is ready.
export function deriveExcerptStarters(
  excerpts: ExcerptChunk[],
  limit: number = 4,
): StarterQuestion[] {
  if (!excerpts || excerpts.length === 0) return [];
  const cleanTitles: string[] = [];
  const seen = new Set<string>();
  for (const e of excerpts) {
    const clean = cleanExcerptTitle(e.source_name);
    if (!clean || clean === "Untitled") continue;
    if (seen.has(clean)) continue;
    seen.add(clean);
    cleanTitles.push(clean);
  }

  const out: StarterQuestion[] = [];

  // Cross-document question first — it's the "unique to svrnmesh"
  // capability and the highest-value question a first-time user
  // could ask. Only offered when we have at least two docs.
  if (cleanTitles.length >= 2) {
    out.push({
      text: formatCrossDocQuestion(cleanTitles),
      atom_id: "excerpt-seed::cross",
      source_section: null,
      question_type: "excerpt_seed",
    });
  }

  // Per-document thesis questions — directly names the doc so
  // the model has the handle it needs.
  for (const title of cleanTitles) {
    if (out.length >= limit) break;
    out.push({
      text: `What's the main argument in ${title}?`,
      atom_id: `excerpt-seed::${title}`,
      source_section: null,
      question_type: "excerpt_seed",
    });
  }

  return out.slice(0, limit);
}

/// Human-readable cross-doc question, capped at 3 named titles
/// with "and N more" for longer lists.
function formatCrossDocQuestion(titles: string[]): string {
  if (titles.length === 2) {
    return `What connects ${titles[0]} and ${titles[1]} — any overlapping themes?`;
  }
  if (titles.length === 3) {
    return `What do ${titles[0]}, ${titles[1]}, and ${titles[2]} have in common?`;
  }
  const rest = titles.length - 2;
  return `What connects ${titles[0]}, ${titles[1]}, and ${rest} other document${rest === 1 ? "" : "s"}?`;
}

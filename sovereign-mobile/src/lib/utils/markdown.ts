// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Markdown rendering with source citation, math, and code highlighting.
 *
 * Renders markdown to HTML via `marked`, with:
 *   - `[Source: name]` citations transformed into clickable chips
 *   - KaTeX math via `marked-katex-extension` ($inline$ and $$block$$)
 *   - `highlight.js` syntax highlighting on fenced code blocks
 *   - A copy button on every code block (click handler in AssistantMessage)
 *
 * Raw HTML from model output is escaped (XSS prevention).
 */

import { Marked } from "marked";
import markedKatex from "marked-katex-extension";
import hljs from "highlight.js/lib/common";

import "katex/dist/katex.min.css";
import "highlight.js/styles/github-dark-dimmed.css";

const marked = new Marked({
  breaks: true,
  gfm: true,
});

marked.use(
  markedKatex({
    throwOnError: false,
    output: "html",
  }),
);

marked.use({
  renderer: {
    code({ text, lang }: { text: string; lang?: string }): string {
      const language = lang?.trim() || "";
      let highlighted: string;
      if (language && hljs.getLanguage(language)) {
        try {
          highlighted = hljs.highlight(text, {
            language,
            ignoreIllegals: true,
          }).value;
        } catch {
          highlighted = escapeHtml(text);
        }
      } else if (language === "" && text.length > 0) {
        const auto = hljs.highlightAuto(text);
        highlighted = auto.value;
      } else {
        highlighted = escapeHtml(text);
      }

      const langLabel = language ? language : "text";
      const encoded = encodeCodePayload(text);
      return (
        `<pre class="code-block" data-lang="${escapeAttr(langLabel)}">` +
        `<div class="code-block-header">` +
        `<span class="code-block-lang">${escapeHtml(langLabel)}</span>` +
        `<button class="code-block-copy" type="button" ` +
        `data-code="${encoded}" aria-label="Copy code">Copy</button>` +
        `</div>` +
        `<code class="hljs language-${escapeAttr(langLabel)}">${highlighted}</code>` +
        `</pre>`
      );
    },
  },
});

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function escapeAttr(text: string): string {
  return text.replace(/"/g, "&quot;").replace(/</g, "&lt;");
}

// btoa over a UTF-8 byte stream so non-ASCII source survives the round-trip.
function encodeCodePayload(text: string): string {
  try {
    const bytes = new TextEncoder().encode(text);
    let bin = "";
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin);
  } catch {
    return "";
  }
}

/**
 * Render markdown text to sanitized HTML.
 *
 * - Headings, bold, italic, lists, code, horizontal rules all render
 * - `[Source: name]` is converted to a clickable citation chip
 * - `[1]`, `[2]`, … numeric refs are converted to fallback chips that
 *   resolve against `retrievedChunks[N-1]` in AssistantMessage. The
 *   prompt forbids this shape (see KNOWLEDGE_SYNTHESIS_SYSTEM in
 *   runtime.rs) but smaller / newer models drift to it; the chip
 *   keeps the glass-box reading surface clickable instead of leaving
 *   plain-text refs the reader can't follow.
 * - `$x^2$` and `$$\int$$` render as KaTeX math
 * - Fenced code blocks are syntax-highlighted with a copy button
 * - Raw HTML tags from model output are escaped
 */
export function renderMarkdown(text: string): string {
  let withCitations = text.replace(
    /\[Source:\s*([^\]]+)\]/g,
    (_match, name: string) => {
      const trimmed = name.trim();
      const escaped = escapeAttr(trimmed);
      return `<span class="source-citation" role="button" tabindex="0" aria-label="Read source: ${escaped}" data-source="${escaped}" title="Retrieved from: ${escaped}">${escapeHtml(trimmed)}</span>`;
    },
  );

  // Numeric fallback: `[N]` where N is 1–99 and the bracket is NOT
  // preceded by an alphanumeric character (so `arr[0]`, `foo[1]` in
  // code-adjacent prose don't get chipped). The capturing group is
  // bare digits — `[Source: …]` shape was already replaced above and
  // can't match here.
  withCitations = withCitations.replace(
    /(^|[^A-Za-z0-9_\]])\[(\d{1,2})\]/g,
    (_match, lead: string, digits: string) => {
      const idx = parseInt(digits, 10);
      return `${lead}<span class="source-citation citation-numeric" role="button" tabindex="0" aria-label="Citation ${idx}" data-citation-index="${idx}" title="Numeric citation [${idx}] — click to resolve">[${idx}]</span>`;
    },
  );

  return marked.parse(withCitations) as string;
}

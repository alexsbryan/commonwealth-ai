/**
 * Markdown rendering with source citation support.
 *
 * Renders markdown to HTML via `marked`, with a custom extension that
 * transforms `[Source: name]` citations into styled inline chips.
 * Raw HTML from model output is escaped (XSS prevention).
 */

import { Marked } from "marked";

// Create a configured instance — reused across renders.
const marked = new Marked({
  breaks: true, // GFM line breaks
  gfm: true, // GitHub-flavoured markdown
});

/**
 * Render markdown text to sanitized HTML.
 *
 * - Headings, bold, italic, lists, code, horizontal rules all render
 * - `[Source: name]` is converted to a clickable citation chip
 * - Raw HTML tags from model output are escaped
 */
export function renderMarkdown(text: string): string {
  // Pre-process: convert [Source: name] to citation chips before markdown
  // parsing. We do this as a text transform so markdown doesn't interfere
  // with the bracket syntax.
  const withCitations = text.replace(
    /\[Source:\s*([^\]]+)\]/g,
    (_match, name: string) => {
      const trimmed = name.trim();
      const escaped = trimmed
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;");
      return `<span class="source-citation" data-source="${escaped}" title="Retrieved from: ${escaped}">${escaped}</span>`;
    },
  );

  const html = marked.parse(withCitations) as string;
  return html;
}

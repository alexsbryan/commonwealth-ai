// SPDX-License-Identifier: AGPL-3.0-or-later
import type { PositionStyle } from "./types";

export type BlockType = "paragraph" | "think" | "research_gap";

export interface ContentBlock {
  type: BlockType;
  text: string;
  /** Position attribution for this paragraph, if detected. */
  position?: { name: string; style: PositionStyle };
  /** For research gaps: the suggested search query. */
  gapQuery?: string;
}

const THINK_OPEN = "<think>";
const THINK_CLOSE = "</think>";

/** Patterns that indicate source-adequacy bookkeeping rather than substantive reasoning. */
const ADMINISTRATIVE_PATTERNS = [
  /^(\*\*)?source analysis/i,
  /^(\*\*)?critical problem/i,
  /no substantive content/i,
  /\[unverified\]/i,
  /cannot fabricate/i,
  /passage.*insufficient/i,
  /passage.*does not contain/i,
  /source.*does not (address|contain|mention|discuss)/i,
  /i cannot (find|provide|fabricate)/i,
  /no relevant (results|content|information|passages)/i,
];

/**
 * Filter administrative source-adequacy bookkeeping from thinking content.
 * If >60% of lines are filtered, returns empty string (suppress the block entirely).
 */
function filterAdministrativeThinking(text: string): string {
  const lines = text.split("\n");
  const totalNonEmpty = lines.filter((l) => l.trim().length > 0).length;
  if (totalNonEmpty === 0) return text;

  const filtered = lines.filter((line) => {
    const trimmed = line.trim();
    if (trimmed.length === 0) return true; // preserve blank lines
    return !ADMINISTRATIVE_PATTERNS.some((pat) => pat.test(trimmed));
  });

  const keptNonEmpty = filtered.filter((l) => l.trim().length > 0).length;
  const removedRatio = 1 - keptNonEmpty / totalNonEmpty;

  // If >60% of substantive lines were removed, suppress the entire block.
  if (removedRatio > 0.6) return "";

  return filtered.join("\n").trim();
}

/** Known position prefixes → style mapping. */
const POSITION_MAP: Record<string, { name: string; style: PositionStyle }> = {
  "[Compatibilism]": {
    name: "Compatibilism",
    style: "Compatibilism",
  },
  "[Hard Incompatibilism]": {
    name: "Hard Incompatibilism",
    style: "HardIncompatibilism",
  },
  "[Libertarianism]": {
    name: "Libertarianism",
    style: "Libertarianism",
  },
};

/**
 * Parse assistant message content into structured blocks.
 *
 * Handles:
 * - <think>...</think> blocks → ThinkBlock type
 * - Double-newline paragraph splitting
 * - Position badge detection: [Compatibilism], [Hard Incompatibilism], [Libertarianism]
 * - Research gap detection: [Research Gap] prefix
 * - Streaming-safe: unclosed <think> treated as in-progress think block
 */
export function parseAssistantContent(content: string): ContentBlock[] {
  const blocks: ContentBlock[] = [];

  // Stray `</think>` repair: when the server-side stream emitted a
  // closing think tag without a preceding `<think>` (model's chat
  // template was rendered with `enable_thinking: false` but the model
  // emitted CoT anyway, OR the THINK_BUDGET enforcement injected a
  // close into a stream that never opened), synthesise a leading
  // `<think>` so the content before the stray close folds into a
  // thinking block instead of leaking into the user-visible text.
  // The substantive answer that follows the stray close still renders
  // as paragraphs unchanged.
  let remaining = content;
  if (
    remaining.includes(THINK_CLOSE) &&
    !remaining.includes(THINK_OPEN)
  ) {
    remaining = THINK_OPEN + remaining;
  }

  // Extract think blocks first.
  while (true) {
    const openIdx = remaining.indexOf(THINK_OPEN);
    if (openIdx === -1) break;

    // Text before the think block
    const before = remaining.slice(0, openIdx).trim();
    if (before) {
      blocks.push(...splitIntoParagraphs(before));
    }

    const closeIdx = remaining.indexOf(THINK_CLOSE, openIdx);
    if (closeIdx === -1) {
      // Unclosed think block — streaming in progress (don't filter yet).
      const thinkContent = remaining.slice(openIdx + THINK_OPEN.length);
      blocks.push({ type: "think", text: thinkContent });
      remaining = "";
      break;
    }

    // Complete think block — filter administrative bookkeeping.
    const rawThink = remaining.slice(
      openIdx + THINK_OPEN.length,
      closeIdx,
    );
    const filteredThink = filterAdministrativeThinking(rawThink.trim());
    if (filteredThink.length > 0) {
      blocks.push({ type: "think", text: filteredThink });
    }
    remaining = remaining.slice(closeIdx + THINK_CLOSE.length);
  }

  // Process remaining text after all think blocks.
  const trimmed = remaining.trim();
  if (trimmed) {
    blocks.push(...splitIntoParagraphs(trimmed));
  }

  return blocks;
}

/** Split text on double-newline into paragraph blocks, detecting position badges and research gaps. */
function splitIntoParagraphs(text: string): ContentBlock[] {
  const paragraphs = text.split(/\n\n+/).filter((p) => p.trim().length > 0);
  return paragraphs.map((raw) => {
    const trimmed = raw.trim();

    // Check for research gap.
    if (
      trimmed.startsWith("[Research Gap]") ||
      trimmed.startsWith("Research gap:")
    ) {
      const colonIdx = trimmed.indexOf(":");
      const gapText =
        colonIdx > -1 ? trimmed.slice(colonIdx + 1).trim() : trimmed;
      return {
        type: "research_gap" as const,
        text: gapText,
        gapQuery: gapText.split(".")[0]?.trim(),
      };
    }

    // Check for position badge prefix.
    for (const [prefix, pos] of Object.entries(POSITION_MAP)) {
      if (trimmed.startsWith(prefix)) {
        return {
          type: "paragraph" as const,
          text: trimmed.slice(prefix.length).trim(),
          position: pos,
        };
      }
    }

    return { type: "paragraph" as const, text: trimmed };
  });
}

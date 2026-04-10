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

  // Extract think blocks first.
  let remaining = content;
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
      // Unclosed think block — streaming in progress.
      const thinkContent = remaining.slice(openIdx + THINK_OPEN.length);
      blocks.push({ type: "think", text: thinkContent });
      remaining = "";
      break;
    }

    // Complete think block.
    const thinkContent = remaining.slice(
      openIdx + THINK_OPEN.length,
      closeIdx,
    );
    blocks.push({ type: "think", text: thinkContent.trim() });
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

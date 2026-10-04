// SPDX-License-Identifier: AGPL-3.0-or-later
// Types for reel-style.mjs — the module is plain ESM so the node-side
// exporter can import it; this keeps the TypeScript beats honest about
// its shape.
export declare const REEL: {
  readonly width: number;
  readonly height: number;
  readonly bg: string;
  readonly fps: number;
};

export declare const FONT_STACK: string;

export declare const CAPTION: {
  readonly fontPx: number;
  readonly weight: number;
  readonly lineHeight: number;
  readonly bottomPx: number;
  readonly maxWidthPct: number;
  readonly padY: number;
  readonly padX: number;
  readonly radius: number;
  readonly bg: string;
  readonly fg: string;
  readonly blurPx: number;
  readonly shadow: string;
  readonly fadeInMs: number;
  readonly fadeOutMs: number;
  readonly holdMs: number;
};

export declare const CAPTION_EL_ID: string;

export declare function captionChipCss(opts?: {
  backdropBlur?: boolean;
  visible?: boolean;
  maskPlate?: boolean;
}): string;

export declare function captionOverlayHtml(
  text: string,
  opts?: { fontDataUri?: string | null; maskPlate?: boolean },
): string;

// SPDX-License-Identifier: AGPL-3.0-or-later
// The reel's visual language — ONE definition, two renderers.
//
// A demo reel is a set of clips that must read as one artifact. Two of
// them are produced by completely different machinery:
//
//   screencast beats  Playwright records the live app; captions are real
//                     DOM injected by `BeatRun.caption()`.
//   raw beats         a human screen-records something Playwright cannot
//                     reach (the sandboxed mesh-app window; a Pi across
//                     the room); captions are burned in by ffmpeg after
//                     the fact (tests/e2e/scripts/demo-export.mjs).
//
// If those two paths each carry their own idea of "the caption style",
// they drift, and the reel looks assembled rather than authored. So the
// geometry, the type, and the chip live here and BOTH paths read them.
// The offscreen renderer literally rasterizes the same CSS string the
// live overlay sets, in a viewport the same size as the frame, so a
// burned-in caption is the same pixels the app would have drawn.
//
// Plain .mjs (with a hand-written .d.mts) rather than .ts because
// demo-export.mjs is a node script and cannot import TypeScript. The
// alternative — two copies and a "keep in sync" comment — is the exact
// failure this file exists to prevent.

/** Frame geometry every clip is normalized to. Must equal the viewport
 *  in playwright.demo.config.ts: the screencast is CSS-pixel and only
 *  ever scales DOWN, so a mismatch letterboxes the app into a corner. */
export const REEL = {
  width: 1280,
  height: 800,
  /** Letterbox / pillarbox fill for a raw take whose aspect isn't 16:10.
   *  Near-black rather than pure black so a padded clip reads as framing
   *  and not as a broken encode. */
  bg: "#0e0e12",
  /** Raw takes are normalized to this. Screen recorders default to 60;
   *  the screencast lands around 25. One rate across the reel keeps the
   *  ladder's bitrate ceilings comparable clip to clip. */
  fps: 30,
};

/** The app's own sans stack (src/app.css `--font-sans`). Naming only
 *  'IBM Plex Sans' here would silently fall back to system-ui — the app
 *  registers the VARIABLE family, and that is what the UI in frame is
 *  set in. */
export const FONT_STACK =
  "'IBM Plex Sans Variable', 'IBM Plex Sans', system-ui, -apple-system, sans-serif";

/** Lower-third caption chip. Every number here is used by both
 *  renderers; change it once and both move together. */
export const CAPTION = {
  fontPx: 20,
  weight: 500,
  lineHeight: 1.4,
  /** Distance from the bottom of the FRAME, not the app content. */
  bottomPx: 44,
  maxWidthPct: 76,
  padY: 12,
  padX: 22,
  radius: 12,
  bg: "rgba(14,14,18,.82)",
  fg: "#f4f4f6",
  /** Frosted backdrop. The live overlay gets this free from CSS
   *  `backdrop-filter`; the exporter reproduces it by blurring the frame
   *  through the chip's own alpha mask, so the two match including the
   *  rounded corners. */
  blurPx: 14,
  shadow: "0 8px 30px rgba(0,0,0,.35)",
  fadeInMs: 320,
  fadeOutMs: 400,
  /** Default time a caption stays up when the sheet doesn't say. */
  holdMs: 2800,
};

/**
 * The chip's `style.cssText`, identical for the live overlay and the
 * offscreen rasterizer.
 *
 * @param {{ backdropBlur?: boolean, visible?: boolean, maskPlate?: boolean }} [opts]
 *   `backdropBlur` off for the offscreen render — there is nothing
 *   behind a transparent canvas to blur, and the exporter applies the
 *   real blur to the video frame instead. `visible` starts the chip at
 *   full opacity (the rasterizer wants the settled state, the live
 *   overlay animates into it). `maskPlate` draws the chip's SHAPE in
 *   opaque white with no text and no shadow — the exporter needs the
 *   region CSS `backdrop-filter` would blur, which is the element's
 *   box, not the box's 82%-opaque paint. Masking with the rendered
 *   alpha instead composites the blurred backdrop at 82% and comes out
 *   visibly darker than the live chip (measured: 12 dB against the DOM
 *   render, where the rest of the frame sat at 41).
 * @returns {string}
 */
export function captionChipCss(opts = {}) {
  const { backdropBlur = true, visible = false, maskPlate = false } = opts;
  if (maskPlate) {
    return [
      "position:fixed",
      "left:50%",
      `bottom:${CAPTION.bottomPx}px`,
      "transform:translateX(-50%) translateY(0)",
      `max-width:${CAPTION.maxWidthPct}%`,
      `padding:${CAPTION.padY}px ${CAPTION.padX}px`,
      `border-radius:${CAPTION.radius}px`,
      "background:#ffffff",
      "color:transparent",
      `font:${CAPTION.weight} ${CAPTION.fontPx}px/${CAPTION.lineHeight} ${FONT_STACK}`,
      "letter-spacing:.005em",
      "text-align:center",
      "opacity:1",
    ].join(";");
  }
  return [
    "position:fixed",
    "left:50%",
    `bottom:${CAPTION.bottomPx}px`,
    visible
      ? "transform:translateX(-50%) translateY(0)"
      : "transform:translateX(-50%) translateY(8px)",
    `max-width:${CAPTION.maxWidthPct}%`,
    `padding:${CAPTION.padY}px ${CAPTION.padX}px`,
    `border-radius:${CAPTION.radius}px`,
    `background:${CAPTION.bg}`,
    ...(backdropBlur ? [`backdrop-filter:blur(${CAPTION.blurPx}px)`] : []),
    `color:${CAPTION.fg}`,
    `font:${CAPTION.weight} ${CAPTION.fontPx}px/${CAPTION.lineHeight} ${FONT_STACK}`,
    "letter-spacing:.005em",
    "text-align:center",
    "pointer-events:none",
    "z-index:2147483645",
    visible ? "opacity:1" : "opacity:0",
    `transition:opacity ${CAPTION.fadeInMs}ms ease, transform ${CAPTION.fadeInMs}ms ease`,
    `box-shadow:${CAPTION.shadow}`,
  ].join(";");
}

/** The id the live overlay uses, so a re-caption replaces rather than stacks. */
export const CAPTION_EL_ID = "__sovereign_demo_caption__";

/**
 * A standalone document that draws ONE settled caption on a transparent
 * canvas the size of the frame. The exporter screenshots this with
 * `omitBackground` to get an RGBA overlay plate.
 *
 * The font is inlined as a data: URI rather than linked, because a
 * file:// page cannot load a file:// webfont in Chromium (CORS applies
 * to fonts) — and a caption that silently rendered in Helvetica would
 * defeat the entire point of this module.
 *
 * @param {string} text
 * @param {{ fontDataUri?: string | null, maskPlate?: boolean }} [opts]
 * @returns {string}
 */
export function captionOverlayHtml(text, opts = {}) {
  const { fontDataUri = null, maskPlate = false } = opts;
  const esc = (s) =>
    String(s).replace(
      /[&<>"']/g,
      (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
    );
  const face = fontDataUri
    ? `@font-face{font-family:'IBM Plex Sans Variable';src:url(${fontDataUri}) format('woff2-variations');font-weight:100 700;font-style:normal;font-display:block}`
    : "";
  return `<!doctype html><meta charset="utf-8"><style>
${face}
html,body{margin:0;padding:0;width:${REEL.width}px;height:${REEL.height}px;background:transparent;overflow:hidden}
</style><div id="${CAPTION_EL_ID}" style="${captionChipCss({ backdropBlur: false, visible: true, maskPlate })}">${esc(text)}</div>`;
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// The synthetic cursor + human-cadence input primitives.
//
// Playwright's recordVideo has no OS cursor in frame — CDP screencast
// composites the page, not the desktop. Rather than fall back to hand
// screen-recording (which is what costs the repeatability), we draw the
// cursor IN the page and drive it with eased motion. That buys the one
// thing paid recorders sell and free ones don't: smooth, deliberate
// pointer paths. Ours are also identical on every take.
//
// Everything here is presentation-only. No assertion depends on it, and
// the overlay is `pointer-events: none` so it can never intercept a real
// click. If the overlay failed to install the beat still runs; it just
// looks worse (never wrong).
import type { Locator, Page } from "@playwright/test";

/** Where the pointer currently is, per page. Playwright's mouse has no
 *  position getter, so we shadow it — glide() needs an origin to ease
 *  FROM, and jumping to (0,0) between beats reads as a teleport. */
const POS = new WeakMap<Page, { x: number; y: number }>();

const OVERLAY_ID = "__sovereign_demo_cursor__";

/** Injected into the page: a dot + halo that tracks mousemove, and a
 *  ripple on mousedown. Written as a string for addInitScript so it
 *  survives navigation (mesh apps navigate away from the app shell). */
function overlayScript(): string {
  return `
(() => {
  if (window.__sovereignDemoCursor) return;
  window.__sovereignDemoCursor = true;
  const install = () => {
    if (document.getElementById(${JSON.stringify(OVERLAY_ID)})) return;
    const el = document.createElement("div");
    el.id = ${JSON.stringify(OVERLAY_ID)};
    el.style.cssText = [
      "position:fixed", "left:0", "top:0", "width:22px", "height:22px",
      "margin:-11px 0 0 -11px", "border-radius:50%", "pointer-events:none",
      "z-index:2147483647", "opacity:0", "transition:opacity 180ms ease",
      "background:radial-gradient(circle at 50% 50%, rgba(255,255,255,.95) 0 22%, rgba(255,255,255,.35) 23% 46%, rgba(255,255,255,0) 47%)",
      "box-shadow:0 0 0 1.25px rgba(20,20,25,.55), 0 2px 10px rgba(0,0,0,.35)",
      "will-change:transform",
    ].join(";");
    document.documentElement.appendChild(el);

    const ripple = () => {
      const r = document.createElement("div");
      const p = window.__sovereignDemoCursorPos || { x: -100, y: -100 };
      r.style.cssText = [
        "position:fixed", "left:" + p.x + "px", "top:" + p.y + "px",
        "width:14px", "height:14px", "margin:-7px 0 0 -7px",
        "border-radius:50%", "pointer-events:none", "z-index:2147483646",
        "border:2px solid rgba(255,255,255,.85)",
        "transform:scale(.4)", "opacity:.9",
        "transition:transform 420ms cubic-bezier(.2,.7,.3,1), opacity 420ms ease",
      ].join(";");
      document.documentElement.appendChild(r);
      requestAnimationFrame(() => {
        r.style.transform = "scale(2.6)";
        r.style.opacity = "0";
      });
      setTimeout(() => r.remove(), 500);
    };

    window.addEventListener("mousemove", (e) => {
      window.__sovereignDemoCursorPos = { x: e.clientX, y: e.clientY };
      el.style.opacity = "1";
      el.style.transform = "translate3d(" + e.clientX + "px," + e.clientY + "px,0)";
    }, { capture: true, passive: true });
    window.addEventListener("mousedown", ripple, { capture: true, passive: true });
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", install, { once: true });
  } else {
    install();
  }
  // Re-install if a framework nukes documentElement children.
  setInterval(install, 1000);
})();
`;
}

/** Install the overlay for the lifetime of the page (survives goto). */
export async function installCursor(page: Page): Promise<void> {
  await page.addInitScript(overlayScript());
  // The page may already be open (fixtures order); inject once now too.
  await page.evaluate(overlayScript()).catch(() => {
    /* no document yet — the init script covers it */
  });
  POS.set(page, { x: 640, y: 700 });
}

/** easeInOutCubic — slow start, slow stop. The stillness at each end is
 *  what makes the motion read as deliberate rather than mechanical. */
function ease(t: number): number {
  return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
}

export interface GlideOptions {
  /** Total travel time. Longer for long distances reads better. */
  durationMs?: number;
  /** Intermediate mouse.move() calls. 30 is smooth at 25fps capture. */
  steps?: number;
}

/** Move the pointer to a point with eased motion. */
export async function glideTo(
  page: Page,
  x: number,
  y: number,
  opts: GlideOptions = {},
): Promise<void> {
  const from = POS.get(page) ?? { x: 640, y: 700 };
  const dist = Math.hypot(x - from.x, y - from.y);
  // Scale duration with distance (clamped): a 40px nudge shouldn't take
  // as long as a corner-to-corner sweep.
  const duration = opts.durationMs ?? Math.min(900, Math.max(260, dist * 1.1));
  const steps = opts.steps ?? Math.max(12, Math.round(duration / 24));
  for (let i = 1; i <= steps; i += 1) {
    const t = ease(i / steps);
    await page.mouse.move(from.x + (x - from.x) * t, from.y + (y - from.y) * t);
    await page.waitForTimeout(duration / steps);
  }
  POS.set(page, { x, y });
}

/** Move the pointer to a locator's centre. Waits for it to be visible
 *  and stable first — gliding to a box that's still animating lands the
 *  cursor next to the target, which looks like a miss. */
export async function glideToLocator(
  page: Page,
  locator: Locator,
  opts: GlideOptions = {},
): Promise<void> {
  await locator.scrollIntoViewIfNeeded();
  await locator.waitFor({ state: "visible" });
  const box = await locator.boundingBox();
  if (!box) return; // off-screen / zero-size: skip the flourish, not the beat
  await glideTo(page, box.x + box.width / 2, box.y + box.height / 2, opts);
}

/** Glide, pause, then click. The beat of stillness before the click is
 *  the single biggest readability win in a demo clip — the viewer's eye
 *  needs to arrive before the UI changes. */
export async function demoClick(
  page: Page,
  locator: Locator,
  opts: GlideOptions & { settleMs?: number } = {},
): Promise<void> {
  await glideToLocator(page, locator, opts);
  await page.waitForTimeout(opts.settleMs ?? 320);
  await locator.click();
}

export interface TypeOptions {
  /** Mean per-character delay. ~34ms reads as a fast, confident typist. */
  charDelayMs?: number;
  /** Extra pause after sentence-ending punctuation. */
  sentencePauseMs?: number;
  /** Extra pause at a blank line — the "thinking between paragraphs" beat. */
  paragraphPauseMs?: number;
  /** Click the field first (and glide to it). */
  focusFirst?: boolean;
}

/** Type text at human cadence, with pauses where a person would pause.
 *
 *  Deliberately NOT `locator.fill()` — fill() is what the correctness
 *  suite uses (instant, deterministic) and it's exactly wrong on camera:
 *  text appearing all at once reads as a script, not a session. */
export async function demoType(
  page: Page,
  locator: Locator,
  text: string,
  opts: TypeOptions = {},
): Promise<void> {
  const charDelay = opts.charDelayMs ?? 34;
  const sentencePause = opts.sentencePauseMs ?? 260;
  const paragraphPause = opts.paragraphPauseMs ?? 900;

  if (opts.focusFirst !== false) {
    await demoClick(page, locator, { settleMs: 180 });
  }

  // Split on paragraph breaks so we can hold between them; within a
  // paragraph, split on sentence ends for a shorter breath.
  const paragraphs = text.split("\n\n");
  for (let p = 0; p < paragraphs.length; p += 1) {
    const sentences = paragraphs[p].split(/(?<=[.!?—])\s+/);
    for (let s = 0; s < sentences.length; s += 1) {
      await locator.pressSequentially(sentences[s], { delay: charDelay });
      if (s < sentences.length - 1) {
        await locator.pressSequentially(" ", { delay: charDelay });
        await page.waitForTimeout(sentencePause);
      }
    }
    if (p < paragraphs.length - 1) {
      await locator.press("Enter");
      await locator.press("Enter");
      await page.waitForTimeout(paragraphPause);
    }
  }
}

/** Park the cursor somewhere neutral so it isn't hovering a tooltip
 *  during a long read beat. */
export async function parkCursor(page: Page): Promise<void> {
  const vp = page.viewportSize();
  if (!vp) return;
  await glideTo(page, vp.width - 60, vp.height - 50, { durationMs: 500 });
}

// Injected via Playwright's addInitScript AFTER tauri-shim.js. Records,
// in page-time, when the chat surface first paints each tier of
// "intelligence signal" the user can see between submitting a query
// and receiving content.
//
// Tiers (timestamped first-match-wins):
//   • generic   — first .typing-indicator       (any "we got your input")
//   • specific  — first .doc-progress-indicator (loading-slot specific)
//   • aux       — first .narration-stack |
//                       .interpretation-banner |
//                       .clarification-card    (auxiliary specific)
//   • visible   — first specific OR aux element whose pixels actually
//                 enter the viewport (IntersectionObserver) — DOM
//                 presence ≠ user-visible
//   • thinking  — first .think-block paint. Models stream <think>...
//                 </think> tokens BEFORE prose; the user sees a
//                 "Reasoning ▶" toggle long before any answer. Without
//                 this tier `content` would understate first-content by
//                 the entire thinking duration.
//   • content   — first non-empty .sv-ai-msg .sv-prose (TTFT)
//
// Derived tier (computed on read):
//   • gap       — content − specific, when both fire. The user-perceived
//                 wait window between "we have something specific to
//                 say" and "actual content arrives". Catches the
//                 "staring at one calm sentence" failure mode.
//   • staleness — max ms the loading-slot text was static (no change).
//                 Bounds "sentence-stare": even when the slot has
//                 specific text, how long did the user see the same
//                 exact text without any update? Lower is better;
//                 unbounded is the failure mode the rotation fix targets.
//
// All measurements are `performance.now() - t0`, where `t0` is set
// inside the page via `markStart()` immediately before the Send
// click. Measuring inside the page avoids Playwright's IPC jitter
// (which can spike to 100-300ms under parallel-worker load).
//
// Vanilla .js because Playwright's addInitScript loads it as a
// classic script.
(() => {
  if (window.__ttfi__) return;

  const SELECTORS = {
    generic: ".typing-indicator",
    specific: ".doc-progress-indicator",
    aux: ".narration-stack, .interpretation-banner, .clarification-card",
    thinking: ".think-block",
    content: ".sv-ai-msg .sv-prose",
  };

  // Per-element selectors observed for the `visible` tier. We watch
  // each leaf indicator/chip rather than the .narration-stack
  // container, so a stack that scrolls partially out of view doesn't
  // count as "visible" while the actual chip is below the fold.
  const VISIBLE_SELECTORS =
    ".doc-progress-indicator, .narration-chip, .interpretation-banner, .clarification-card";

  // The slot text the user reads. Tracked for the staleness tier.
  const SLOT_TEXT_SELECTOR = ".doc-progress-indicator .progress-text";

  let t0 = null;
  let report = {
    generic: null,
    specific: null,
    aux: null,
    visible: null,
    thinking: null,
    content: null,
  };
  let mutationObserver = null;
  let intersectionObserver = null;
  let observedElements = new WeakSet();

  // Slot-text history for staleness computation. Each entry is the
  // exact text the user saw at that timestamp. We never coalesce or
  // truncate — the metric depends on capturing every change.
  let slotTextHistory = [];
  // Set when the slot leaves the DOM (or its text empties); marks the
  // close-time of the last static window for the trailing computation.
  let slotEndTs = null;

  function elementCounts(el, key) {
    if (key === "content") {
      // .sv-prose is rendered when proseText is non-empty, but it can
      // appear briefly with whitespace-only text (e.g. word-buffer
      // flushing a leading space). Require trimmed length > 0.
      return (el.textContent ?? "").trim().length > 0;
    }
    if (key === "specific") {
      // .doc-progress-indicator is rendered as a wrapper; only count
      // when its progress-text is non-empty so a stylistic stub doesn't
      // skew the metric.
      const text = el.querySelector?.(".progress-text");
      if (!text) return true;
      return (text.textContent ?? "").trim().length > 0;
    }
    return true;
  }

  function sweepKey(key) {
    if (report[key] != null) return;
    const matches = document.querySelectorAll(SELECTORS[key]);
    for (const el of matches) {
      if (elementCounts(el, key)) {
        report[key] = performance.now() - t0;
        return;
      }
    }
  }

  function attachVisibleWatch() {
    if (report.visible != null) return;
    if (!intersectionObserver) return;
    const candidates = document.querySelectorAll(VISIBLE_SELECTORS);
    for (const el of candidates) {
      if (!observedElements.has(el)) {
        observedElements.add(el);
        intersectionObserver.observe(el);
      }
    }
  }

  function trackSlotText() {
    if (t0 == null) return;
    const el = document.querySelector(SLOT_TEXT_SELECTOR);
    if (!el) {
      // Slot left the DOM. Close the trailing window if we'd seen
      // anything; further sweeps after this are no-ops for staleness.
      if (slotTextHistory.length > 0 && slotEndTs == null) {
        slotEndTs = performance.now() - t0;
      }
      return;
    }
    const text = (el.textContent ?? "").trim();
    if (!text) return;
    slotEndTs = null; // Slot is back / still up.
    const last = slotTextHistory[slotTextHistory.length - 1];
    if (!last || last.text !== text) {
      slotTextHistory.push({ ts: performance.now() - t0, text });
    }
  }

  function computeStaleness() {
    if (slotTextHistory.length === 0) return null;
    let max = 0;
    for (let i = 1; i < slotTextHistory.length; i++) {
      const delta = slotTextHistory[i].ts - slotTextHistory[i - 1].ts;
      if (delta > max) max = delta;
    }
    // Closing window: from last text change to slot disappearance,
    // or to "now" if the slot is still up at read time. The trailing
    // window is real user-perceived staleness too.
    const close =
      slotEndTs != null ? slotEndTs : performance.now() - t0;
    const trailing =
      close - slotTextHistory[slotTextHistory.length - 1].ts;
    if (trailing > max) max = trailing;
    return max;
  }

  function sweep() {
    if (t0 == null) return;
    sweepKey("generic");
    sweepKey("specific");
    sweepKey("aux");
    sweepKey("thinking");
    sweepKey("content");
    attachVisibleWatch();
    trackSlotText();
  }

  function onIntersect(entries) {
    if (report.visible != null) return;
    for (const entry of entries) {
      // threshold 0 means any pixel of overlap. We additionally require
      // a non-zero bounding rect so a display:none element (which would
      // have zero size) doesn't false-fire.
      if (entry.isIntersecting && entry.intersectionRect.width > 0 && entry.intersectionRect.height > 0) {
        report.visible = performance.now() - t0;
        // First-match wins; stop observing — but DON'T disconnect the
        // observer outright in case markStart() runs again on the same
        // page-load. Just unobserve everything we know about.
        for (const e of entries) intersectionObserver.unobserve(e.target);
        return;
      }
    }
  }

  window.__ttfi__ = {
    /** Anchor t=0. Must be called immediately before the click that
     *  starts the turn. Resets all markers and starts observing the DOM. */
    markStart() {
      t0 = performance.now();
      report = {
        generic: null,
        specific: null,
        aux: null,
        visible: null,
        thinking: null,
        content: null,
      };
      slotTextHistory = [];
      slotEndTs = null;
      observedElements = new WeakSet();
      if (mutationObserver) mutationObserver.disconnect();
      if (intersectionObserver) intersectionObserver.disconnect();
      mutationObserver = new MutationObserver(() => sweep());
      mutationObserver.observe(document.body, {
        childList: true,
        subtree: true,
        characterData: true,
      });
      intersectionObserver = new IntersectionObserver(onIntersect, {
        threshold: 0,
      });
      // Initial pass in case any markers already exist (shouldn't on a
      // fresh page, but cheap defence).
      sweep();
    },
    /** Read the current report. Any tier still null means the marker
     *  hasn't appeared yet. `gap` is derived: content − specific (null
     *  if either is missing). `staleness` is the longest static-text
     *  window over the slot's lifetime, computed from the slot text
     *  history captured during sweeps. */
    getReport() {
      const r = { ...report };
      r.gap =
        r.content != null && r.specific != null ? r.content - r.specific : null;
      r.staleness = computeStaleness();
      return r;
    },
    /** Read the t0 anchor (page-time ms). Used by scenario-player to
     *  schedule events relative to the click. */
    getT0() {
      return t0;
    },
    /** Disconnect observers. The page tear-down handles this on
     *  navigation, but tests can call it explicitly. */
    reset() {
      t0 = null;
      report = {
        generic: null,
        specific: null,
        aux: null,
        visible: null,
        thinking: null,
        content: null,
      };
      slotTextHistory = [];
      slotEndTs = null;
      mutationObserver?.disconnect();
      mutationObserver = null;
      intersectionObserver?.disconnect();
      intersectionObserver = null;
      observedElements = new WeakSet();
    },
  };
})();

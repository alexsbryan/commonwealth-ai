// Injected via Playwright's addInitScript AFTER tauri-shim.js. Records,
// in page-time, when the chat surface first paints each tier of
// "intelligence signal" the user can see between submitting a query
// and receiving content.
//
// Tiers:
//   • generic   — first .typing-indicator       (any "we got your input")
//   • specific  — first .doc-progress-indicator (loading-slot specific)
//   • aux       — first .narration-stack |
//                       .interpretation-banner |
//                       .clarification-card    (auxiliary specific)
//   • content   — first non-empty .sv-ai-msg .sv-prose (TTFT)
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
    content: ".sv-ai-msg .sv-prose",
  };

  let t0 = null;
  let report = { generic: null, specific: null, aux: null, content: null };
  let observer = null;

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

  function sweep() {
    if (t0 == null) return;
    sweepKey("generic");
    sweepKey("specific");
    sweepKey("aux");
    sweepKey("content");
  }

  window.__ttfi__ = {
    /** Anchor t=0. Must be called immediately before the click that
     *  starts the turn. Resets all markers and starts observing the DOM. */
    markStart() {
      t0 = performance.now();
      report = { generic: null, specific: null, aux: null, content: null };
      if (observer) observer.disconnect();
      observer = new MutationObserver(() => sweep());
      observer.observe(document.body, {
        childList: true,
        subtree: true,
        characterData: true,
      });
      // Initial pass in case any markers already exist (shouldn't on a
      // fresh page, but cheap defence).
      sweep();
    },
    /** Read the current report. Any tier still null means the marker
     *  has not yet appeared. */
    getReport() {
      return { ...report };
    },
    /** Read the t0 anchor (page-time ms). Used by scenario-player to
     *  schedule events relative to the click. */
    getT0() {
      return t0;
    },
    /** Disconnect the observer. The page tear-down handles this on
     *  navigation, but tests can call it explicitly. */
    reset() {
      t0 = null;
      report = { generic: null, specific: null, aux: null, content: null };
      observer?.disconnect();
      observer = null;
    },
  };
})();

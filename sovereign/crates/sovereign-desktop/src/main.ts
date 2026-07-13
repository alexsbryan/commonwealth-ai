// SPDX-License-Identifier: AGPL-3.0-or-later
// IBM Plex Sans Variable — chrome and outer-work face (ss03+ss05+calt at weight 420)
import "@fontsource-variable/ibm-plex-sans";
// Source Serif 4 Variable with optical-size axis — body and inner-work face (opsz 14, weight 380)
import "@fontsource-variable/source-serif-4/opsz.css";
import "@fontsource-variable/source-serif-4/opsz-italic.css";
// IBM Plex Mono — shares family DNA with Plex Sans; replaces Syne Mono
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { diagnoseCorpus } from "./lib/api";
// TTFI recorder — inert unless ?ttfi=record or localStorage flag is
// set. Self-binds to window.__ttfi_recorder__ on import.
import "./lib/ttfi/recorder";

// Global safety net for uncaught async failures. Without this, any
// un-awaited rejecting promise (a dropped `invoke`, a listener callback
// that throws) surfaces as a raw, unshaped "Uncaught (in promise)" in
// devtools with no context — and there's no frontend equivalent of the
// Rust `install_panic_hook` crash record. We shape it into a single
// tagged line so it's identifiable in a user's console and greppable in
// a screen-share, and leave the hook where a future crash-capture can
// forward it to the same local crash store the Rust side uses.
window.addEventListener("unhandledrejection", (event) => {
  const reason = event.reason;
  const message =
    reason instanceof Error ? `${reason.name}: ${reason.message}` : String(reason);
  console.error("[unhandled-rejection]", message);
  // TODO(crash-capture): forward to the local crash store (crash_report)
  // so frontend async failures are as recoverable as Rust panics.
});
window.addEventListener("error", (event) => {
  console.error("[uncaught-error]", event.message);
});

const app = mount(App, {
  target: document.getElementById("app")!,
});

// Expose diagnostic for debugging from the browser console:
//   await window.diagnoseCorpus()
(window as any).diagnoseCorpus = async () => {
  const report = await diagnoseCorpus();
  console.log(report);
  return report;
};

export default app;

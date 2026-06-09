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

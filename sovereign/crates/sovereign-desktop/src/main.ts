import "@fontsource/syne/400.css";
import "@fontsource/syne/500.css";
import "@fontsource/syne/600.css";
import "@fontsource/syne/700.css";
import "@fontsource/syne/800.css";
import "@fontsource/syne-mono/400.css";
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

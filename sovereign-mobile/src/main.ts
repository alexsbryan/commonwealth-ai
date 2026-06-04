import { mount } from "svelte";
import App from "./App.svelte";
// Lavender Court fonts — same fontsource bundle the desktop ships.
import "@fontsource-variable/ibm-plex-sans";
import "@fontsource-variable/source-serif-4/opsz.css";
import "@fontsource-variable/source-serif-4/opsz-italic.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./app.css";

const app = mount(App, { target: document.getElementById("app")! });

// ── TEMP overflow diagnostic — renders an on-screen overlay listing any
// element extending past the viewport right edge (WKWebView doesn't
// forward console.log). Remove after the responsiveness pass. ──
const ovf = document.createElement("pre");
ovf.id = "ovf-debug";
ovf.style.cssText =
  "position:fixed;top:50px;left:4px;right:4px;z-index:99999;margin:0;" +
  "padding:6px;background:rgba(0,0,0,0.86);color:#7CFF7C;font:9px/1.25 monospace;" +
  "white-space:pre-wrap;pointer-events:none;border:1px solid #7CFF7C;max-height:46vh;overflow:hidden;";
document.body.appendChild(ovf);
setInterval(() => {
  const vw = document.documentElement.clientWidth;
  const offenders = Array.from(document.querySelectorAll("*"))
    .filter((el) => el.id !== "ovf-debug")
    .map((el) => ({ el, r: el.getBoundingClientRect() }))
    .filter(({ r }) => r.width > 0 && r.right > vw + 0.5)
    .sort((a, b) => b.r.right - a.r.right)
    .slice(0, 9);
  const lines = [`vw=${vw} docScrollW=${document.documentElement.scrollWidth} past=${offenders.length}`];
  for (const { el, r } of offenders) {
    const e = el as HTMLElement;
    const cls = (e.className || "").toString().slice(0, 20);
    lines.push(`! ${e.tagName.toLowerCase()}.${cls} R${Math.round(r.right)} W${Math.round(r.width)}`);
  }
  // Internal horizontal scrollers (content wider than the box → side-scroll).
  const scrollers = Array.from(document.querySelectorAll("*"))
    .filter((el) => el.id !== "ovf-debug")
    .map((el) => el as HTMLElement)
    .filter((e) => e.scrollWidth > e.clientWidth + 1 && e.clientWidth > 40)
    .sort((a, b) => b.scrollWidth - b.clientWidth - (a.scrollWidth - a.clientWidth))
    .slice(0, 5);
  for (const e of scrollers) {
    const cls = (e.className || "").toString().slice(0, 18);
    lines.push(`>> ${e.tagName.toLowerCase()}.${cls} cW${e.clientWidth} sW${e.scrollWidth}`);
  }
  // Gutters of key elements (L = left gap, Rg = right gap to vw).
  const probe = ["#app", ".chat", ".scroll", ".user", ".composer", ".composer button", ".cites", ".list"];
  for (const sel of probe) {
    const e = document.querySelector(sel) as HTMLElement | null;
    if (!e) continue;
    const r = e.getBoundingClientRect();
    lines.push(`${sel} L${Math.round(r.left)} Rg${Math.round(vw - r.right)} W${Math.round(r.width)}`);
  }
  ovf.textContent = lines.join("\n");
}, 1500);

export default app;

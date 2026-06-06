// Enron identity & counterparty explorer — composed from the MeshApp SDK.
//
// All host access is the permission-gated `window.meshApp` bridge (no
// inference). This file is now just composition: the SDK owns the scale banner,
// force-graph, timeline, reconciliation reveal, search, and cited drill-down;
// here we wire them to the Enron corpus and its on-ramp. The previous ~600-line
// hand-rolled version lives in git history.

import {
  $, connect, hasBridge, emsg, fmtInt,
  scaleBanner, threadList, forceGraph, typeToggle, timelineChart, monthLabel,
  reconciliationList, searchBox, entityDetail, citationExpander, el,
} from "../_sdk/meshapp.js";

const CORPUS = "enron-sample-multi-wide";
let bridge;

// On-ramp: real people resolved by search at load, led by their description.
const THREADS = [
  { q: "Kenneth Lay", type: "person" },
  { q: "Jeff Skilling", type: "person" },
  { q: "Fastow", type: "person" },
  { q: "Dynegy", type: "institution" },
];

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available — the host bridge shim did not load.");
  bridge = connect(CORPUS);
  $("source").textContent = "Source: " + CORPUS;

  // Probe the load-bearing op — if it fails, the corpus isn't installed/granted.
  try {
    await bridge.subgraph("institution", 1);
  } catch (e) {
    return fail(
      "Bridge call failed: " + emsg(e) +
      "  (is the Enron app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }
  $("loading").hidden = true;
  $("app").hidden = false;

  loadBanner();
  loadThreads();
  loadMap("institution");
  loadTimeline();
  loadReconciliation();

  typeToggle($("map-toggle"), [
    { type: "institution", label: "Companies" },
    { type: "person", label: "People" },
    { type: "all", label: "All" },
  ], { initial: "institution", onChange: loadMap });

  searchBox($("search-host"), bridge, {
    placeholder: "e.g. Fastow, Dynegy, Calpine, LJM",
    ariaLabel: "Search entities",
    onPick: openEntity,
  });
}

async function loadBanner() {
  let s;
  try { s = await bridge.corpusStats(); } catch { return; }
  scaleBanner($("banner"), [
    { num: fmtInt(s.documents), cap: "emails" }, "→",
    { num: fmtInt(s.entities), cap: "people & companies" },
    { num: fmtInt(s.edges), cap: "relationships" },
    { num: fmtInt(s.reconciled_merges), cap: "identities merged" },
    { num: fmtInt(s.claims), cap: "claims extracted" },
    { num: "0", cap: "humans read them", glow: true },
  ]);
}

async function loadThreads() {
  const hits = await Promise.all(THREADS.map(async (t) => {
    try { return pickBest(await bridge.search(t.q, t.type, 4), t.q); } catch { return null; }
  }));
  threadList($("threads"), hits, { onPick: openEntity });
}

function pickBest(hits, q) {
  if (!hits || !hits.length) return null;
  const ql = q.toLowerCase();
  return (
    hits.find((h) => h.canonical_name.toLowerCase() === ql) ||
    hits.find((h) => h.canonical_name.toLowerCase().includes(ql)) ||
    hits[0]
  );
}

async function loadMap(type) {
  const msg = $("map-msg");
  msg.textContent = "";
  let g;
  try { g = await bridge.subgraph(type === "all" ? null : type, 40); }
  catch (e) { msg.textContent = "map failed: " + emsg(e); return; }
  const nodes = (g && g.nodes) || [];
  if (!nodes.length) {
    $("map").replaceChildren();
    msg.textContent = "no graph for this type.";
    return;
  }
  forceGraph($("map"), g, { onNodeClick: openEntity });
  msg.textContent = nodes.length + " nodes · " + ((g.edges || []).length) + " links · drag a node, click to open.";
}

async function loadTimeline() {
  const msg = $("timeline-msg");
  $("timeline").replaceChildren();
  $("timeline-detail").replaceChildren();
  msg.textContent = "";
  let tl;
  try { tl = await bridge.timeline(); } catch (e) { msg.textContent = "timeline unavailable: " + emsg(e); return; }
  const buckets = (tl && tl.buckets) || [];
  if (!buckets.length) { msg.textContent = "no dated documents in this corpus."; return; }
  timelineChart($("timeline"), buckets, { onMonth: showMonth });
  msg.textContent = fmtInt(tl.dated) + " of " + fmtInt(tl.total) + " documents dated. Click a month.";
}

function showMonth(b) {
  const box = $("timeline-detail");
  box.replaceChildren();
  box.appendChild(el("div", { class: "meta", text: b.count + " emails in " + monthLabel(b.ym) + " — read a few as they landed:" }));
  for (const id of (b.chunk_ids || []).slice(0, 8)) {
    box.appendChild(citationExpander(bridge, id, { label: "email" }));
  }
}

async function loadReconciliation() {
  const msg = $("merges-msg");
  $("merges").replaceChildren();
  msg.textContent = "";
  let merges;
  try { merges = await bridge.reconciliation(); } catch (e) { msg.textContent = "reconciliation unavailable: " + emsg(e); return; }
  if (!merges || !merges.length) { msg.textContent = "no reconciliation merges recorded for this corpus."; return; }
  msg.textContent = merges.length + " cross-inbox merges.";
  reconciliationList($("merges"), merges, { onOpen: openEntity });
}

async function openEntity(id) {
  let node;
  try { node = await bridge.node(id); }
  catch (e) { const m = $("search-msg"); if (m) m.textContent = "load failed: " + emsg(e); return; }
  entityDetail($("detail"), node, { bridge, onOpen: openEntity, citationLabel: "the source email" });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

// SPDX-License-Identifier: AGPL-3.0-or-later
// ════════════════════════════════════════════════════════════════════════
//  COPY-PASTE TEMPLATE — a complete mesh app you can make your own.
//
//  This is a real, working explorer over the "federalist-starter" corpus.
//  To build one for YOUR corpus:
//    1. Copy this folder:  public/meshapp/federalist/ → public/meshapp/<your-app>/
//    2. In meshapp.json: change "id", "name", "blurb", "corpus".
//    3. Below: change CORPUS to your corpus id, then edit the copy + views.
//    4. Reorder/remove views, or compose new ones from ../_sdk/meshapp.js.
//
//  Everything talks to the host through the permission-gated `window.meshApp`
//  bridge (read-only: mesh_store_read). No inference, no network — the SDK
//  owns the rendering; this file is just composition. Look for EDIT.
// ════════════════════════════════════════════════════════════════════════

import {
  $, connect, hasBridge, emsg, fmtInt,
  scaleBanner, forceGraph, typeToggle, searchBox, entityDetail,
  claimList, questionList,
} from "../_sdk/meshapp.js";

// EDIT: your corpus id (the recipe you authored + built).
const CORPUS = "federalist-starter";

let bridge;

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available — the host bridge shim did not load.");
  bridge = connect(CORPUS);
  $("source").textContent = "Source: " + CORPUS;

  // Probe the load-bearing op (also drives the type toggle). A failure here
  // means the corpus isn't built/installed or this app isn't granted to read it.
  let g0;
  try {
    g0 = await bridge.graph(null, 200);
  } catch (e) {
    return fail(
      "Couldn't load the atlas — " + emsg(e) +
      "  (make sure the corpus is built and this app is allowed to read it.)"
    );
  }
  $("loading").hidden = true;
  $("app").hidden = false;

  // ── The views. Reorder, remove, or add — each is one SDK call. ──
  loadBanner();        // a view: headline counts
  buildTypeToggle(g0); // a view: filter the map by entity type
  loadMap(null);       // a view: the entity force-graph
  loadClaims();        // a view: the arguments (claim atoms)
  loadQuestions();     // a view: open questions (question atoms)

  searchBox($("search-host"), bridge, {
    placeholder: "Search entities by name",   // EDIT
    ariaLabel: "Search entities",
    onPick: openEntity,
  });

  $("door").addEventListener("click", () => {
    // Opens Outer Work (the main window's chat) scoped to this corpus.
    bridge.openOuterWork().catch((e) => {
      $("door").textContent = "couldn't open chat: " + emsg(e);
    });
  });
}

async function loadBanner() {
  let s;
  try { s = await bridge.corpusStats(); } catch { return; }
  // EDIT: pick the counts that matter for your corpus.
  const items = [
    { num: fmtInt(s.entities), cap: "entities" },
    { num: fmtInt(s.edges), cap: "connections" },
    { num: fmtInt(s.claims), cap: "arguments" },
    { num: fmtInt(s.questions), cap: "questions" },
  ].filter((it) => it.num !== "0");
  if (items.length) scaleBanner($("banner"), items);
}

// The map's type filter is derived from the entity types actually present —
// no hard-coded categories, so it works for any domain. Hidden when there's
// only one type.
function buildTypeToggle(g) {
  const types = [...new Set((g.nodes || []).map((n) => n.entity_type).filter(Boolean))];
  if (types.length <= 1) {
    $("map-toggle").hidden = true;
    return;
  }
  const opts = [{ type: "all", label: "All" }, ...types.slice(0, 6).map((t) => ({ type: t, label: t }))];
  typeToggle($("map-toggle"), opts, {
    initial: "all",
    onChange: (t) => loadMap(t === "all" ? null : t),
  });
}

async function loadMap(type) {
  const msg = $("map-msg");
  msg.textContent = "";
  let g;
  try { g = await bridge.subgraph(type, 40); }
  catch (e) { msg.textContent = "map failed: " + emsg(e); return; }
  const nodes = (g && g.nodes) || [];
  if (!nodes.length) {
    $("map").replaceChildren();
    msg.textContent = "no entities for this type.";
    return;
  }
  forceGraph($("map"), g, { onNodeClick: openEntity });
  msg.textContent = nodes.length + " entities · " + ((g.edges || []).length) + " links · drag a node, click to open.";
}

async function loadClaims() {
  let claims = [];
  try { claims = await bridge.claims(60); }
  catch (e) { $("claims").textContent = "claims unavailable: " + emsg(e); return; }
  claimList($("claims"), claims, { empty: "no claims extracted for this corpus." });
}

async function loadQuestions() {
  let questions = [];
  try { questions = await bridge.questions(60); }
  catch (e) { $("questions").textContent = "questions unavailable: " + emsg(e); return; }
  questionList($("questions"), questions, { empty: "no questions raised in this corpus." });
}

async function openEntity(id) {
  let node;
  try { node = await bridge.node(id); }
  catch (e) { const m = $("search-msg"); if (m) m.textContent = "load failed: " + emsg(e); return; }
  // EDIT: citationLabel is what each cited source is called in your domain.
  entityDetail($("detail"), node, { bridge, onOpen: openEntity, citationLabel: "the source passage" });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

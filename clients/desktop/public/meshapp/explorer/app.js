// SPDX-License-Identifier: AGPL-3.0-or-later
// Generic Atlas Explorer — composed from the MeshApp SDK over ANY corpus's
// atlas. Unlike the per-domain bundles (enron, lvt), the corpus is bound at
// launch via `?corpus=<id>` rather than a hard-coded constant, so one bundle
// serves every recipe a user authors. All host access is the permission-gated
// `window.meshApp` bridge (mesh_store_read only; no inference). The host opens
// it with `open_corpus_explorer(corpusId)`.

import {
  $, connect, hasBridge, emsg, fmtInt,
  scaleBanner, forceGraph, typeToggle, searchBox, entityDetail,
  claimList, questionList,
} from "../_sdk/meshapp.js";

const CORPUS = new URLSearchParams(location.search).get("corpus") || "";
let bridge;

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available — the host bridge shim did not load.");
  if (!CORPUS) return fail("No corpus was specified. Launch this explorer from a corpus.");
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

  loadBanner();
  buildTypeToggle(g0);
  loadMap(null);
  loadClaims();
  loadQuestions();

  searchBox($("search-host"), bridge, {
    placeholder: "Search entities by name",
    ariaLabel: "Search entities",
    onPick: openEntity,
  });

  $("door").addEventListener("click", () => {
    bridge.openOuterWork().catch((e) => {
      $("door").textContent = "couldn't open chat: " + emsg(e);
    });
  });
}

async function loadBanner() {
  let s;
  try { s = await bridge.corpusStats(); } catch { return; }
  const items = [
    { num: fmtInt(s.entities), cap: "entities" },
    { num: fmtInt(s.edges), cap: "connections" },
    { num: fmtInt(s.claims), cap: "claims" },
    { num: fmtInt(s.questions), cap: "questions" },
  ].filter((it) => it.num !== "0");
  if (items.length) scaleBanner($("banner"), items);
}

/** Build the entity-type toggle from the types actually present (no hard-coded
 *  "person"/"institution" — this is a generic explorer). Hidden when there's
 *  only one type to show. */
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
  entityDetail($("detail"), node, { bridge, onOpen: openEntity, citationLabel: "the source" });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

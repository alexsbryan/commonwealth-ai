// Project Blue Book evidence explorer — composed from the MeshApp SDK.
//
// Reads the host's deterministic investigation graph through the permission-
// gated `window.meshApp` bridge (no inference). It composes the SDK's shared
// pieces — search box, cited-edge rows, the citation expander — but keeps its
// own hotspot ranking (a fold over typed pattern findings) and its
// auto-surfaced primary Form-10073 card, which are Blue-Book-specific. That's
// the SDK as a toolkit: take the shared parts, add what's yours.

import { $, connect, hasBridge, emsg, el, barList, searchBox, citedEdge } from "../_sdk/meshapp.js";

const CORPUS = "uap-blue-book";
let bridge;

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available — the host bridge shim did not load.");
  bridge = connect(CORPUS);
  let findings;
  try {
    findings = await bridge.findings("sighting_hotspots");
  } catch (e) {
    return fail(
      "Bridge call failed: " + emsg(e) +
      "  (is the Blue Book app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }

  $("loading").hidden = true;
  $("app").hidden = false;
  $("source").textContent = "Source: " + CORPUS;

  renderHotspots(findings);
  searchBox($("search-host"), bridge, {
    placeholder: "e.g. Kirtland, Wright-Patterson, Los Alamos",
    ariaLabel: "Search installations",
    nodeType: "installation",
    onPick: loadEntity,
  });
}

// Each `sighting_hotspots` finding is one installation + a sighting_count in
// `attributes.value`. Rank by count; click → drill in.
function renderHotspots(findings) {
  const rows = (findings || [])
    .map((f) => ({ ent: (f.entities && f.entities[0]) || null, count: Number((f.attributes && f.attributes.value) || 0) }))
    .filter((r) => r.ent)
    .sort((a, b) => b.count - a.count)
    .map((r) => ({ id: r.ent.id, name: r.ent.canonical_name, value: r.count }));
  barList($("hotspots"), rows, { onPick: loadEntity, countSuffix: " unexplained", empty: "no hotspot findings in this corpus." });
}

// Drill into one entity: attributes, folded OCR aliases, the primary card, and
// every incident cited edge (each resolved to its other endpoint + quoting its
// card). The detail shape is Blue-Book-specific; the edges are SDK `citedEdge`s.
async function loadEntity(id) {
  let node;
  try {
    node = await bridge.node(id);
  } catch (e) {
    const m = $("search-msg");
    if (m) m.textContent = "load failed: " + emsg(e);
    return;
  }

  $("detail").hidden = false;
  $("d-type").textContent = node.entity_type;
  $("d-name").textContent = node.canonical_name;
  const attrs = Object.entries(node.attributes || {}).map(([k, v]) => k + ": " + v).join("  ·  ");
  const aliasNote = node.aliases && node.aliases.length ? node.aliases.length + " folded OCR variant(s)" : "";
  $("d-meta").textContent = [attrs, aliasNote].filter(Boolean).join("  ·  ");
  const n = node.edges ? node.edges.length : 0;
  $("d-edgecount").textContent = n + (n === 1 ? " cited edge" : " cited edges");

  renderPrimaryCard(node);

  const box = $("edges");
  box.replaceChildren();
  if (n === 0) box.appendChild(el("div", { class: "meta", text: "no edges recorded for this entity." }));
  for (const e of node.edges || []) {
    box.appendChild(citedEdge(bridge, e, { onOpen: loadEntity, citationLabel: "the full card" }));
  }
  $("detail").scrollIntoView({ behavior: "smooth", block: "start" });
}

// Auto-surface the primary source card for a narrative entity (case/sighting):
// the chunk most of its edges cite. Installations span many cards → no primary.
function renderPrimaryCard(node) {
  const pc = $("primary-card");
  pc.replaceChildren();
  const narrative = node.entity_type === "case" || node.entity_type === "sighting";
  if (!narrative || !node.edges || !node.edges.length) return;
  const counts = {};
  for (const e of node.edges) if (e.source_chunk) counts[e.source_chunk] = (counts[e.source_chunk] || 0) + 1;
  const primary = Object.keys(counts).sort((a, b) => counts[b] - counts[a])[0];
  if (!primary) return;

  pc.appendChild(el("div", { class: "label", style: { marginTop: "10px" } },
    "The Air Force's record card ",
    el("span", { class: "chip", text: "Form-10073 · chunk " + primary })));
  const body = el("pre", { class: "card-full", text: "loading card " + primary + "…" });
  pc.appendChild(body);
  bridge.readChunk(primary)
    .then((ch) => { body.textContent = ch && ch.content ? ch.content : "(card is empty)"; })
    .catch((err) => { body.textContent = "could not load card: " + emsg(err); });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

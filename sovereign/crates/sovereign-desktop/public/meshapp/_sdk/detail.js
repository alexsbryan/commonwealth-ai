// MeshApp SDK — cited drill-down: the glassbox primitives.
//
// Every relationship in an explorer dereferences to the source document it was
// extracted from. These render that contract: an expandable citation, a cited
// edge row, and a full entity-detail panel. `entityDetail` is a convenience for
// the common shape (type / name / description / reconciliation / aliases /
// cited edges); apps with a bespoke detail compose `citedEdge` +
// `citationExpander` directly (the UAP Blue Book does, to auto-surface a card).

import { el, clear, emsg } from "./dom.js";
import { describe } from "./bridge.js";

/**
 * A collapsed "read the source" toggle that lazily fetches a chunk's full text.
 * `label` lets a bundle name the source (e.g. "the source email", "the full
 * card"); `suffix` is trailing text on the toggle line (e.g. a confidence note).
 */
export function citationExpander(bridge, chunkId, opts = {}) {
  const label = opts.label || "the source";
  const wrap = el("div");
  const line = el("div", { class: "prov" });
  const toggle = el("button", { type: "button", class: "link" });
  const full = el("pre", { class: "card-full" });
  full.hidden = true;
  const text = (open) => (open ? "▾ hide " : "▸ read ") + label + " (chunk " + chunkId + ")";
  toggle.textContent = text(false);
  line.appendChild(toggle);
  if (opts.suffix) line.appendChild(document.createTextNode(opts.suffix));
  let loaded = false;
  toggle.addEventListener("click", async () => {
    if (!loaded) {
      toggle.textContent = "loading " + label + " " + chunkId + "…";
      try {
        const ch = await bridge.readChunk(chunkId);
        full.textContent = ch && ch.content ? ch.content : "(empty)";
      } catch (e) {
        full.textContent = "could not load: " + emsg(e);
      }
      loaded = true;
      full.hidden = false;
    } else {
      full.hidden = !full.hidden;
    }
    toggle.textContent = text(!full.hidden);
  });
  wrap.appendChild(line);
  wrap.appendChild(full);
  return wrap;
}

/**
 * One cited relationship row: the typed relation → its other endpoint
 * (clickable, drives `onOpen`), an optional one-line description (Event atoms),
 * the verbatim excerpt, and the citation expander for the whole source.
 */
export function citedEdge(bridge, e, opts = {}) {
  const onOpen = opts.onOpen || (() => {});
  const citationLabel = opts.citationLabel || "the source";
  const d = el("div", { class: "edge" });

  const other = el("button", {
    type: "button", class: "link",
    text: e.other_name + (e.other_type ? " (" + e.other_type + ")" : ""),
  });
  other.addEventListener("click", () => onOpen(e.other_id));
  d.appendChild(el("div", { class: "edge-head" },
    el("span", { class: "rel", text: e.relationship_type }),
    " " + (e.direction === "out" ? "→" : "←") + " ",
    other));

  const desc = e.attributes && typeof e.attributes.description === "string" ? e.attributes.description : "";
  if (desc) d.appendChild(el("div", { class: "meta", text: desc }));
  if (e.excerpt) d.appendChild(el("div", { class: "excerpt", text: "“" + e.excerpt + "”" }));

  if (e.source_chunk) {
    const conf = typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "";
    d.appendChild(citationExpander(bridge, e.source_chunk, { label: citationLabel, suffix: conf }));
  } else {
    d.appendChild(el("div", { class: "prov",
      text: "no source chunk" + (typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "") }));
  }
  return d;
}

/**
 * Render an entity's full detail into `container` (the `#detail` card): its
 * description, reconciliation provenance, folded aliases, and every cited edge.
 * Builds the `#d-type/#d-name/#d-desc/#d-recon/#d-edgecount/#edges` structure.
 */
export function entityDetail(container, node, opts = {}) {
  const onOpen = opts.onOpen || (() => {});
  clear(container);
  container.hidden = false;
  container.appendChild(el("div", { class: "ent-type", id: "d-type", text: node.entity_type || "" }));
  container.appendChild(el("div", { class: "ent-name", id: "d-name", text: node.canonical_name || "" }));
  const desc = describe(node);
  container.appendChild(el("div", { class: "ent-desc", id: "d-desc", text: desc ? "“" + desc + "”" : "" }));

  const recon = el("div", { id: "d-recon" });
  const r = node.attributes && node.attributes.reconciliation;
  if (r && Array.isArray(r.surface_forms) && r.surface_forms.length) {
    const signals = (r.signals_fired || []).join(", ") || "name match";
    recon.appendChild(el("div", { class: "recon-box" },
      el("div", { text: "Reconciled identity — folded " + r.surface_forms.length + " surface forms (signal: " + signals + ")" }),
      el("div", { class: "forms", text: r.surface_forms.join("  ·  ") })));
  }
  if (node.aliases && node.aliases.length) {
    recon.appendChild(el("div", { class: "alias-list",
      text: "Also seen as: " + node.aliases.slice(0, 12).join("  ·  ") + (node.aliases.length > 12 ? "  · …" : "") }));
  }
  container.appendChild(recon);

  const n = node.edges ? node.edges.length : 0;
  container.appendChild(el("div", { class: "label", style: { marginTop: "14px" } },
    "Cited relationships ",
    el("span", { class: "chip", id: "d-edgecount", text: n + (n === 1 ? " cited link" : " cited links") })));
  const edges = el("div", { id: "edges" });
  if (n === 0) edges.appendChild(el("div", { class: "meta", text: "no cited relationships recorded for this entity." }));
  for (const e of node.edges || []) {
    edges.appendChild(citedEdge(opts.bridge, e, { onOpen, citationLabel: opts.citationLabel }));
  }
  container.appendChild(edges);
  container.scrollIntoView({ behavior: "smooth", block: "start" });
}

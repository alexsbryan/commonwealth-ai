// Enron identity & counterparty explorer — a first-party mesh app.
//
// Reads the host's deterministic ATLAS (entities + reconciliation + the
// entity-to-entity edge graph) through the permission-gated `window.meshApp`
// bridge. There is NO inference here: the centrality ranking is graph degree
// over the atlas edges, the reconciliation list is the merge log verbatim, and
// every relationship drill-down quotes the source EMAIL it was extracted from
// (verbatim excerpt + chunk id). The bundle's only channel to the host is
// `window.meshApp`; the same DTO contract the UAP Blue Book explorer codes
// against, here backed by an atlas instead of an investigation graph.

const CORPUS = "enron-sample-multi-wide";
const $ = (id) => document.getElementById(id);

async function main() {
  if (!window.meshApp) {
    return fail("window.meshApp is not available — the host bridge shim did not load.");
  }
  $("source").textContent = "Source: " + CORPUS;

  // The counterparty graph is the load-bearing view: if it fails, the corpus
  // isn't installed / granted, so surface that rather than a half-empty UI.
  try {
    await loadGraph("institution");
  } catch (e) {
    return fail(
      "Bridge call failed: " + (e && e.message ? e.message : e) +
      "  (is the Enron app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }

  $("loading").hidden = true;
  $("app").hidden = false;

  // Reconciliation is tolerant — a corpus with no merge log just hides the list.
  loadReconciliation();

  for (const btn of document.querySelectorAll("#type-toggle button")) {
    btn.addEventListener("click", () => {
      for (const b of document.querySelectorAll("#type-toggle button")) {
        b.setAttribute("aria-pressed", b === btn ? "true" : "false");
      }
      loadGraph(btn.dataset.type);
    });
  }
  $("search").addEventListener("click", () => doSearch($("q").value.trim()));
  $("q").addEventListener("keydown", (e) => {
    if (e.key === "Enter") doSearch($("q").value.trim());
  });
}

// Degree-ranked entities of one type (or all). Degree = incident cited
// relationships, so the bar length is "how central to the correspondence."
async function loadGraph(type) {
  const box = $("graph");
  box.replaceChildren();
  let nodes;
  try {
    nodes = await window.meshApp.graph(CORPUS, type === "all" ? null : type, 40);
  } catch (e) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "graph failed: " + (e && e.message ? e.message : e);
    box.appendChild(m);
    if (type === "institution") throw e; // surfaced as a hard failure on first load
    return;
  }
  const rows = (nodes || []).filter((n) => n.degree > 0);
  if (rows.length === 0) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "no graph edges for this type.";
    box.appendChild(m);
    return;
  }
  const max = rows.reduce((m, r) => Math.max(m, r.degree), 1);
  for (const r of rows) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "hot";
    const left = document.createElement("div");
    const nm = document.createElement("div");
    nm.textContent = r.canonical_name + (r.alias_count ? "  ·  " + r.alias_count + " surface forms" : "");
    const bar = document.createElement("div");
    bar.className = "bar";
    bar.style.width = Math.max(8, Math.round((r.degree / max) * 100)) + "%";
    left.appendChild(nm);
    left.appendChild(bar);
    const cnt = document.createElement("div");
    cnt.className = "cnt";
    cnt.textContent = r.degree + " links";
    btn.appendChild(left);
    btn.appendChild(cnt);
    btn.addEventListener("click", () => loadEntity(r.id));
    box.appendChild(btn);
  }
}

// The cross-origin merge log: each canonical entity + the surface forms folded
// into it + the signal that fired. This is the glassbox — "every merge carries
// its reason." Click a merge to open the canonical entity.
async function loadReconciliation() {
  const box = $("merges");
  const msg = $("merges-msg");
  box.replaceChildren();
  msg.textContent = "";
  let merges;
  try {
    merges = await window.meshApp.reconciliation(CORPUS);
  } catch (e) {
    msg.textContent = "reconciliation unavailable: " + (e && e.message ? e.message : e);
    return;
  }
  if (!merges || merges.length === 0) {
    msg.textContent = "no reconciliation merges recorded for this corpus.";
    return;
  }
  msg.textContent = merges.length + " cross-inbox merges.";
  for (const m of merges) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "merge";
    const canon = document.createElement("div");
    canon.className = "canon";
    canon.textContent = m.canonical_name;
    for (const s of m.signals_fired || []) {
      const chip = document.createElement("span");
      chip.className = "chip signal";
      chip.style.marginLeft = "8px";
      chip.textContent = s;
      canon.appendChild(chip);
    }
    const forms = document.createElement("div");
    forms.className = "forms";
    const arrow = document.createElement("span");
    arrow.className = "arrow";
    arrow.textContent = (m.surface_forms || []).join("  ·  ") + "  →  ";
    forms.appendChild(arrow);
    forms.appendChild(document.createTextNode(m.canonical_name));
    btn.appendChild(canon);
    btn.appendChild(forms);
    btn.addEventListener("click", () => loadEntity(m.canonical_id));
    box.appendChild(btn);
  }
}

async function doSearch(q) {
  $("search-msg").textContent = "";
  $("matches").replaceChildren();
  if (!q) return;
  let hits;
  try {
    hits = await window.meshApp.searchEntities(CORPUS, q, null, 25);
  } catch (e) {
    $("search-msg").textContent = "search failed: " + (e && e.message ? e.message : e);
    return;
  }
  if (!hits || hits.length === 0) {
    $("search-msg").textContent = "no entity matching '" + q + "'";
    return;
  }
  const box = $("matches");
  for (const h of hits) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "match";
    const kind = h.entity_type ? "  ·  " + h.entity_type : "";
    b.textContent = h.canonical_name + kind + "  ·  " + h.degree + " links";
    b.addEventListener("click", () => loadEntity(h.id));
    box.appendChild(b);
  }
}

// Drill into one entity: its reconciliation provenance, folded surface forms,
// and every incident cited edge — each resolved to its other endpoint
// (clickable, so you can walk the graph) and quoting the source email.
async function loadEntity(id) {
  let node;
  try {
    node = await window.meshApp.node(CORPUS, id);
  } catch (e) {
    $("search-msg").textContent = "load failed: " + (e && e.message ? e.message : e);
    return;
  }
  $("detail").hidden = false;
  $("d-type").textContent = node.entity_type;
  $("d-name").textContent = node.canonical_name;

  const attrs = node.attributes || {};
  const desc = typeof attrs.description === "string" ? attrs.description : "";
  $("d-meta").textContent = desc;

  // Reconciliation provenance — WHY this canonical exists, if it was a merge.
  const reconBox = $("d-recon");
  reconBox.replaceChildren();
  const recon = attrs.reconciliation;
  if (recon && Array.isArray(recon.surface_forms) && recon.surface_forms.length) {
    const box = document.createElement("div");
    box.className = "recon-box";
    const head = document.createElement("div");
    const signals = (recon.signals_fired || []).join(", ") || "name match";
    head.textContent =
      "Reconciled identity — folded " + recon.surface_forms.length +
      " surface forms (signal: " + signals + ")";
    const forms = document.createElement("div");
    forms.className = "forms";
    forms.textContent = recon.surface_forms.join("  ·  ");
    box.appendChild(head);
    box.appendChild(forms);
    reconBox.appendChild(box);
  }
  // Coalesce surface forms (distinct from cross-origin reconciliation): the
  // ways this entity is named within its own extractions.
  if (node.aliases && node.aliases.length) {
    const al = document.createElement("div");
    al.className = "alias-list";
    al.textContent = "Also seen as: " + node.aliases.slice(0, 12).join("  ·  ") +
      (node.aliases.length > 12 ? "  · …" : "");
    reconBox.appendChild(al);
  }

  const n = node.edges ? node.edges.length : 0;
  $("d-edgecount").textContent = n + (n === 1 ? " cited link" : " cited links");

  const box = $("edges");
  box.replaceChildren();
  if (n === 0) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "no cited relationships recorded for this entity.";
    box.appendChild(m);
  }
  for (const e of node.edges || []) {
    box.appendChild(renderEdge(e));
  }
  $("detail").scrollIntoView({ behavior: "smooth", block: "start" });
}

function renderEdge(e) {
  const d = document.createElement("div");
  d.className = "edge";

  const head = document.createElement("div");
  head.className = "edge-head";
  const rel = document.createElement("span");
  rel.className = "rel";
  rel.textContent = e.relationship_type;
  head.appendChild(rel);
  head.appendChild(document.createTextNode(" " + (e.direction === "out" ? "→" : "←") + " "));
  const other = document.createElement("button");
  other.type = "button";
  other.className = "link";
  other.textContent = e.other_name + (e.other_type ? " (" + e.other_type + ")" : "");
  other.addEventListener("click", () => loadEntity(e.other_id));
  head.appendChild(other);
  d.appendChild(head);

  // An event edge carries the LLM's one-line description as a label.
  const attrs = e.attributes || {};
  if (typeof attrs.description === "string" && attrs.description) {
    const lbl = document.createElement("div");
    lbl.className = "meta";
    lbl.textContent = attrs.description;
    d.appendChild(lbl);
  }

  if (e.excerpt) {
    const ex = document.createElement("div");
    ex.className = "excerpt";
    ex.textContent = "“" + e.excerpt + "”";
    d.appendChild(ex);
  }

  // The excerpt is only the fragment the extractor tagged; the WHOLE source
  // email sits behind `source_chunk`. Let the reader expand it.
  const prov = document.createElement("div");
  prov.className = "prov";
  if (e.source_chunk) {
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "link";
    const label = (open) =>
      (open ? "▾ hide" : "▸ read") + " the source email (chunk " + e.source_chunk + ")";
    toggle.textContent = label(false);
    const full = document.createElement("pre");
    full.className = "card-full";
    full.hidden = true;
    let loaded = false;
    toggle.addEventListener("click", async () => {
      if (!loaded) {
        toggle.textContent = "loading email " + e.source_chunk + "…";
        try {
          const ch = await window.meshApp.readChunk(CORPUS, String(e.source_chunk));
          full.textContent = ch && ch.content ? ch.content : "(email is empty)";
        } catch (err) {
          full.textContent = "could not load email: " + (err && err.message ? err.message : err);
        }
        loaded = true;
        full.hidden = false;
      } else {
        full.hidden = !full.hidden;
      }
      toggle.textContent = label(!full.hidden);
    });
    prov.appendChild(toggle);
    if (typeof e.confidence === "number") {
      prov.appendChild(document.createTextNode(" · confidence " + e.confidence.toFixed(2)));
    }
    d.appendChild(prov);
    d.appendChild(full);
  } else {
    prov.textContent =
      "no source chunk" +
      (typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "");
    d.appendChild(prov);
  }
  return d;
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

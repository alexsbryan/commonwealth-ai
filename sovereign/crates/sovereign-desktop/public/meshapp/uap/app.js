// Project Blue Book evidence explorer — a first-party mesh app.
//
// Reads the host's deterministic investigation graph through the
// permission-gated `window.meshApp` bridge (findings / searchEntities /
// node). There is NO inference here: the hotspot ranking is a fold over
// typed pattern findings, and every edge in a drill-down quotes the
// Form-10073 card it was extracted from (verbatim excerpt + chunk id).
// The bundle's only channel to the host is `window.meshApp`.

const CORPUS = "uap-blue-book";
const $ = (id) => document.getElementById(id);

async function main() {
  if (!window.meshApp) {
    return fail("window.meshApp is not available — the host bridge shim did not load.");
  }
  let findings;
  try {
    findings = await window.meshApp.findings(CORPUS, "sighting_hotspots");
  } catch (e) {
    return fail(
      "Bridge call failed: " + (e && e.message ? e.message : e) +
      "  (is the Blue Book app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }

  $("loading").hidden = true;
  $("app").hidden = false;
  $("source").textContent = "Source: " + CORPUS;

  renderHotspots(findings);

  $("search").addEventListener("click", () => doSearch($("q").value.trim()));
  $("q").addEventListener("keydown", (e) => {
    if (e.key === "Enter") doSearch($("q").value.trim());
  });
}

// Each `sighting_hotspots` finding is one installation entity + a
// sighting_count in `attributes.value`. Rank by count; click → drill in.
function renderHotspots(findings) {
  const rows = (findings || [])
    .map((f) => ({
      ent: (f.entities && f.entities[0]) || null,
      count: Number((f.attributes && f.attributes.value) || 0),
    }))
    .filter((r) => r.ent)
    .sort((a, b) => b.count - a.count);
  const max = rows.reduce((m, r) => Math.max(m, r.count), 1);
  const box = $("hotspots");
  box.replaceChildren();
  if (rows.length === 0) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "no hotspot findings in this corpus.";
    box.appendChild(m);
    return;
  }
  for (const r of rows) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "hot";
    const left = document.createElement("div");
    const nm = document.createElement("div");
    nm.textContent = r.ent.canonical_name;
    const bar = document.createElement("div");
    bar.className = "bar";
    bar.style.width = Math.max(8, Math.round((r.count / max) * 100)) + "%";
    left.appendChild(nm);
    left.appendChild(bar);
    const cnt = document.createElement("div");
    cnt.className = "cnt";
    cnt.textContent = r.count + " unexplained";
    btn.appendChild(left);
    btn.appendChild(cnt);
    btn.addEventListener("click", () => loadEntity(r.ent.id));
    box.appendChild(btn);
  }
}

async function doSearch(q) {
  $("search-msg").textContent = "";
  $("matches").replaceChildren();
  if (!q) return;
  let hits;
  try {
    hits = await window.meshApp.searchEntities(CORPUS, q, "installation", 25);
  } catch (e) {
    $("search-msg").textContent = "search failed: " + (e && e.message ? e.message : e);
    return;
  }
  if (!hits || hits.length === 0) {
    $("search-msg").textContent = "no installation matching '" + q + "'";
    return;
  }
  const box = $("matches");
  for (const h of hits) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "match";
    b.textContent = h.canonical_name + "  ·  " + h.degree + " links";
    b.addEventListener("click", () => loadEntity(h.id));
    box.appendChild(b);
  }
}

// Drill into one entity: its attributes, folded OCR aliases, and every
// incident edge — each resolved to its other endpoint (clickable, so you
// can walk the graph) and quoting its cited card excerpt + chunk.
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
  const attrs = Object.entries(node.attributes || {})
    .map(([k, v]) => k + ": " + v)
    .join("  ·  ");
  const aliasNote =
    node.aliases && node.aliases.length
      ? node.aliases.length + " folded OCR variant(s)"
      : "";
  $("d-meta").textContent = [attrs, aliasNote].filter(Boolean).join("  ·  ");
  const n = node.edges ? node.edges.length : 0;
  $("d-edgecount").textContent = n + (n === 1 ? " cited edge" : " cited edges");

  const box = $("edges");
  box.replaceChildren();
  if (n === 0) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "no edges recorded for this entity.";
    box.appendChild(m);
  }
  for (const e of node.edges || []) {
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

    if (e.excerpt) {
      const ex = document.createElement("div");
      ex.className = "excerpt";
      ex.textContent = "“" + e.excerpt + "”";
      d.appendChild(ex);
    }
    const prov = document.createElement("div");
    prov.className = "prov";
    prov.textContent =
      "card chunk " + (e.source_chunk || "—") +
      (typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "");
    d.appendChild(prov);

    box.appendChild(d);
  }
  $("detail").scrollIntoView({ behavior: "smooth", block: "start" });
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

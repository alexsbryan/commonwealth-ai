// Enron identity & counterparty explorer — a first-party mesh app.
//
// Reads the host's deterministic ATLAS through the permission-gated
// `window.meshApp` bridge. NO inference: the scale banner, force-graph,
// timeline, and reconciliation list are all folds over atoms + sidecars, and
// every relationship drill-down quotes the source EMAIL it was extracted from.
// The only channel to the host is `window.meshApp`. Built to convey depth to a
// newcomer: it leads with the machine-written descriptions and a guided
// on-ramp, then the web of dealings, the collapse timeline, and the glassbox
// identity merges.

const CORPUS = "enron-sample-multi-wide";
const $ = (id) => document.getElementById(id);
const emsg = (e) => (e && e.message ? e.message : String(e));
const fmt = (n) => (n || 0).toLocaleString("en-US");

async function main() {
  if (!window.meshApp) {
    return fail("window.meshApp is not available — the host bridge shim did not load.");
  }
  $("source").textContent = "Source: " + CORPUS;

  // Probe the load-bearing op first — if it fails the corpus isn't installed /
  // granted, so surface that rather than a half-empty UI.
  try {
    await window.meshApp.subgraph(CORPUS, "institution", 1);
  } catch (e) {
    return fail(
      "Bridge call failed: " + emsg(e) +
      "  (is the Enron app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }

  $("loading").hidden = true;
  $("app").hidden = false;

  // These are independent and tolerant — each renders or quietly degrades.
  loadBanner();
  loadThreads();
  loadMap("institution");
  loadTimeline();
  loadReconciliation();

  for (const btn of document.querySelectorAll("#map-toggle button")) {
    btn.addEventListener("click", () => {
      for (const b of document.querySelectorAll("#map-toggle button")) {
        b.setAttribute("aria-pressed", b === btn ? "true" : "false");
      }
      loadMap(btn.dataset.type);
    });
  }
  $("search").addEventListener("click", () => doSearch($("q").value.trim()));
  $("q").addEventListener("keydown", (e) => {
    if (e.key === "Enter") doSearch($("q").value.trim());
  });
}

// ─── Scale / provenance banner ───────────────────────────────────────
async function loadBanner() {
  let s;
  try {
    s = await window.meshApp.corpusStats(CORPUS);
  } catch {
    return;
  }
  const box = $("banner");
  box.replaceChildren();
  const stat = (num, cap, glow) => {
    const d = document.createElement("div");
    d.className = "stat";
    const n = document.createElement("div");
    n.className = "num" + (glow ? " glow" : "");
    n.textContent = num;
    const c = document.createElement("div");
    c.className = "cap";
    c.textContent = cap;
    d.appendChild(n);
    d.appendChild(c);
    return d;
  };
  const arrow = () => {
    const a = document.createElement("div");
    a.className = "arrow";
    a.textContent = "→";
    return a;
  };
  box.appendChild(stat(fmt(s.documents), "emails"));
  box.appendChild(arrow());
  box.appendChild(stat(fmt(s.entities), "people & companies"));
  box.appendChild(stat(fmt(s.edges), "relationships"));
  box.appendChild(stat(fmt(s.reconciled_merges), "identities merged"));
  box.appendChild(stat(fmt(s.claims), "claims extracted"));
  box.appendChild(stat("0", "humans read them", true));
}

// ─── Guided on-ramp ──────────────────────────────────────────────────
const THREADS = [
  { q: "Kenneth Lay", type: "person" },
  { q: "Jeff Skilling", type: "person" },
  { q: "Fastow", type: "person" },
  { q: "Dynegy", type: "institution" },
];

async function loadThreads() {
  const box = $("threads");
  box.replaceChildren();
  const results = await Promise.all(
    THREADS.map(async (t) => {
      try {
        return pickBest(await window.meshApp.searchEntities(CORPUS, t.q, t.type, 4), t.q);
      } catch {
        return null;
      }
    })
  );
  let any = false;
  for (const r of results) {
    if (!r) continue;
    any = true;
    box.appendChild(threadCard(r));
  }
  if (!any) {
    const m = document.createElement("div");
    m.className = "meta";
    m.textContent = "(featured threads unavailable)";
    box.appendChild(m);
  }
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

function threadCard(h) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "thread";
  const head = document.createElement("div");
  const name = document.createElement("span");
  name.className = "tname";
  name.textContent = h.canonical_name;
  const kind = document.createElement("span");
  kind.className = "tkind";
  kind.textContent = h.entity_type || "";
  head.appendChild(name);
  head.appendChild(kind);
  b.appendChild(head);
  const desc = descOf(h);
  if (desc) {
    const d = document.createElement("div");
    d.className = "tdesc";
    d.textContent = "“" + desc + "”";
    b.appendChild(d);
  }
  b.addEventListener("click", () => loadEntity(h.id));
  return b;
}

const descOf = (n) =>
  n && n.attributes && typeof n.attributes.description === "string" ? n.attributes.description : "";

// ─── Network map (force-directed, CSP-safe SVG) ──────────────────────
async function loadMap(type) {
  const box = $("map");
  const msg = $("map-msg");
  msg.textContent = "";
  let g;
  try {
    g = await window.meshApp.subgraph(CORPUS, type === "all" ? null : type, 40);
  } catch (e) {
    msg.textContent = "map failed: " + emsg(e);
    return;
  }
  const nodes = (g && g.nodes) || [];
  if (!nodes.length) {
    box.replaceChildren();
    const m = document.createElement("div");
    m.className = "meta";
    m.style.padding = "20px";
    m.textContent = "no graph for this type.";
    box.appendChild(m);
    return;
  }
  renderForceGraph(box, g, loadEntity);
  msg.textContent =
    nodes.length + " nodes · " + ((g.edges || []).length) + " links · drag a node, click to open.";
}

// A tiny self-contained force simulation rendered as SVG. Fixed viewBox space
// (so it scales to the container regardless of visibility at render time);
// deterministic spiral init (no Math.random → stable layout each load).
function renderForceGraph(container, data, onNodeClick) {
  container.replaceChildren();
  const W = 760, H = 440;
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
  container.appendChild(svg);
  const tip = document.createElement("div");
  tip.className = "map-tip";
  container.appendChild(tip);

  const nodes = data.nodes.map((n) => ({ ...n, x: 0, y: 0, vx: 0, vy: 0 }));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const links = (data.edges || [])
    .map((e) => ({ s: byId.get(e.source), t: byId.get(e.target) }))
    .filter((l) => l.s && l.t);

  const cx = W / 2, cy = H / 2;
  const GA = Math.PI * (3 - Math.sqrt(5)); // golden angle
  nodes.forEach((n, i) => {
    const r = 14 + Math.sqrt(i) * 26;
    n.x = cx + r * Math.cos(i * GA);
    n.y = cy + r * Math.sin(i * GA);
  });

  const maxDeg = nodes.reduce((m, n) => Math.max(m, n.degree || 1), 1);
  const radius = (n) => 5 + 14 * Math.sqrt((n.degree || 1) / maxDeg);
  const color = (n) =>
    n.entity_type === "person" ? "var(--person)" :
    n.entity_type === "institution" ? "var(--inst)" : "var(--other)";

  const linkEls = links.map(() => {
    const ln = document.createElementNS(NS, "line");
    ln.setAttribute("stroke", "#2c3340");
    ln.setAttribute("stroke-width", "1");
    svg.appendChild(ln);
    return ln;
  });

  let dragging = null, downAt = null, moved = false;
  for (const n of nodes) {
    const g = document.createElementNS(NS, "g");
    const c = document.createElementNS(NS, "circle");
    c.setAttribute("r", radius(n));
    c.setAttribute("fill", color(n));
    c.setAttribute("stroke", "#0c0e12");
    c.setAttribute("stroke-width", "1.5");
    g.appendChild(c);
    if (radius(n) >= 9) {
      const t = document.createElementNS(NS, "text");
      t.setAttribute("class", "nlabel");
      t.setAttribute("text-anchor", "middle");
      const nm = n.canonical_name || n.id;
      t.textContent = nm.length > 22 ? nm.slice(0, 21) + "…" : nm;
      g.appendChild(t);
      n._label = t;
    }
    g.addEventListener("mouseenter", (ev) => showTip(n, ev));
    g.addEventListener("mousemove", (ev) => showTip(n, ev));
    g.addEventListener("mouseleave", () => { tip.style.opacity = "0"; });
    g.addEventListener("pointerdown", (ev) => {
      dragging = n; downAt = { x: ev.clientX, y: ev.clientY }; moved = false;
      reheat();
      if (g.setPointerCapture) g.setPointerCapture(ev.pointerId);
    });
    g.addEventListener("pointermove", (ev) => {
      if (dragging !== n || !downAt) return;
      if (Math.hypot(ev.clientX - downAt.x, ev.clientY - downAt.y) > 4) moved = true;
      const r = svg.getBoundingClientRect();
      n.x = ((ev.clientX - r.left) / r.width) * W;
      n.y = ((ev.clientY - r.top) / r.height) * H;
      paint();
    });
    g.addEventListener("pointerup", () => {
      if (dragging === n && !moved) onNodeClick(n.id);
      dragging = null; downAt = null;
    });
    svg.appendChild(g);
    n._g = g;
  }

  function showTip(n, ev) {
    tip.replaceChildren();
    const nm = document.createElement("div");
    nm.textContent = n.canonical_name + "  ·  " + (n.degree || 0) + " links";
    tip.appendChild(nm);
    const desc = descOf(n);
    if (desc) {
      const d = document.createElement("div");
      d.className = "td";
      d.textContent = desc;
      tip.appendChild(d);
    }
    const r = container.getBoundingClientRect();
    tip.style.left = Math.min(r.width - 200, ev.clientX - r.left + 12) + "px";
    tip.style.top = Math.max(4, ev.clientY - r.top - 8) + "px";
    tip.style.opacity = "1";
  }

  function paint() {
    links.forEach((l, i) => {
      linkEls[i].setAttribute("x1", l.s.x);
      linkEls[i].setAttribute("y1", l.s.y);
      linkEls[i].setAttribute("x2", l.t.x);
      linkEls[i].setAttribute("y2", l.t.y);
    });
    for (const n of nodes) {
      n._g.setAttribute("transform", `translate(${n.x.toFixed(1)},${n.y.toFixed(1)})`);
      if (n._label) n._label.setAttribute("y", -(radius(n) + 3));
    }
  }

  function step() {
    for (let i = 0; i < nodes.length; i++) {
      const a = nodes[i];
      for (let j = i + 1; j < nodes.length; j++) {
        const b = nodes[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let d2 = dx * dx + dy * dy || 0.01;
        const d = Math.sqrt(d2);
        const f = 1600 / d2;
        const ux = dx / d, uy = dy / d;
        a.vx += ux * f; a.vy += uy * f; b.vx -= ux * f; b.vy -= uy * f;
      }
    }
    for (const l of links) {
      let dx = l.t.x - l.s.x, dy = l.t.y - l.s.y;
      const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
      const f = (d - 72) * 0.02;
      const ux = dx / d, uy = dy / d;
      l.s.vx += ux * f; l.s.vy += uy * f; l.t.vx -= ux * f; l.t.vy -= uy * f;
    }
    for (const n of nodes) {
      if (n === dragging) { n.vx = 0; n.vy = 0; continue; }
      n.vx += (cx - n.x) * 0.003;
      n.vy += (cy - n.y) * 0.003;
      n.vx *= 0.86; n.vy *= 0.86;
      n.x += n.vx; n.y += n.vy;
      n.x = Math.max(16, Math.min(W - 16, n.x));
      n.y = Math.max(16, Math.min(H - 16, n.y));
    }
    paint();
  }

  let frames = 0, running = false;
  function loop() {
    running = true;
    step();
    frames++;
    if (frames < 300 || dragging) requestAnimationFrame(loop);
    else running = false;
  }
  function reheat() {
    frames = 0;
    if (!running) requestAnimationFrame(loop);
  }
  requestAnimationFrame(loop);
}

// ─── Collapse timeline ───────────────────────────────────────────────
const MONTHS = ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const fmtYm = (ym) => {
  const [y, m] = String(ym).split("-");
  return (MONTHS[+m] || m) + " '" + (y || "").slice(2);
};

async function loadTimeline() {
  const box = $("timeline");
  const msg = $("timeline-msg");
  box.replaceChildren();
  msg.textContent = "";
  $("timeline-detail").replaceChildren();
  let tl;
  try {
    tl = await window.meshApp.timeline(CORPUS);
  } catch (e) {
    msg.textContent = "timeline unavailable: " + emsg(e);
    return;
  }
  const buckets = (tl && tl.buckets) || [];
  if (!buckets.length) {
    msg.textContent = "no dated documents in this corpus.";
    return;
  }
  const max = buckets.reduce((m, b) => Math.max(m, b.count), 1);
  for (const b of buckets) {
    const col = document.createElement("button");
    col.type = "button";
    col.className = "tl-col";
    col.setAttribute("aria-pressed", "false");
    col.title = b.count + " emails in " + fmtYm(b.ym);
    const bar = document.createElement("div");
    bar.className = "tl-bar";
    bar.style.height = Math.max(2, Math.round((b.count / max) * 100)) + "%";
    const lab = document.createElement("div");
    lab.className = "tl-lab";
    lab.textContent = fmtYm(b.ym);
    col.appendChild(bar);
    col.appendChild(lab);
    col.addEventListener("click", () => {
      for (const c of box.children) c.setAttribute("aria-pressed", "false");
      col.setAttribute("aria-pressed", "true");
      showMonth(b);
    });
    box.appendChild(col);
  }
  msg.textContent = fmt(tl.dated) + " of " + fmt(tl.total) + " documents dated. Click a month.";
}

function showMonth(b) {
  const box = $("timeline-detail");
  box.replaceChildren();
  const head = document.createElement("div");
  head.className = "meta";
  head.textContent = b.count + " emails in " + fmtYm(b.ym) + " — read a few as they landed:";
  box.appendChild(head);
  for (const id of (b.chunk_ids || []).slice(0, 8)) {
    box.appendChild(makeEmailExpander(id, "email"));
  }
}

// ─── Reconciled identities (with reveal) ─────────────────────────────
async function loadReconciliation() {
  const box = $("merges");
  const msg = $("merges-msg");
  box.replaceChildren();
  msg.textContent = "";
  let merges;
  try {
    merges = await window.meshApp.reconciliation(CORPUS);
  } catch (e) {
    msg.textContent = "reconciliation unavailable: " + emsg(e);
    return;
  }
  if (!merges || !merges.length) {
    msg.textContent = "no reconciliation merges recorded for this corpus.";
    return;
  }
  msg.textContent = merges.length + " cross-inbox merges.";
  for (const m of merges) {
    box.appendChild(mergeRow(m));
  }
}

function mergeRow(m) {
  const row = document.createElement("div");
  row.className = "merge";
  const canon = document.createElement("div");
  canon.className = "canon";
  canon.textContent = m.canonical_name;
  canon.style.cursor = "pointer";
  canon.title = "open this entity";
  canon.addEventListener("click", (e) => {
    e.stopPropagation();
    loadEntity(m.canonical_id);
  });
  for (const s of m.signals_fired || []) {
    const chip = document.createElement("span");
    chip.className = "chip signal";
    chip.style.marginLeft = "8px";
    chip.textContent = s;
    canon.appendChild(chip);
  }
  const forms = document.createElement("div");
  forms.className = "forms";
  forms.textContent = (m.surface_forms || []).join("  ·  ") + "  →  " + m.canonical_name;

  // The reveal: surface forms cascade in, then collapse to the canonical.
  const reveal = document.createElement("div");
  reveal.className = "reveal";
  (m.surface_forms || []).forEach((sf, i) => {
    const c = document.createElement("span");
    c.className = "sf";
    c.style.transitionDelay = (i * 0.08).toFixed(2) + "s";
    c.textContent = "“" + sf + "”";
    reveal.appendChild(c);
  });
  const to = document.createElement("span");
  to.className = "to";
  to.textContent = " ⟶ ";
  const canon2 = document.createElement("span");
  canon2.className = "canon2";
  canon2.textContent = m.canonical_name;
  reveal.appendChild(to);
  reveal.appendChild(canon2);

  let open = false;
  row.addEventListener("click", () => {
    open = !open;
    reveal.classList.toggle("on", open);
  });
  row.appendChild(canon);
  row.appendChild(forms);
  row.appendChild(reveal);
  return row;
}

// ─── Search ──────────────────────────────────────────────────────────
async function doSearch(q) {
  $("search-msg").textContent = "";
  $("matches").replaceChildren();
  if (!q) return;
  let hits;
  try {
    hits = await window.meshApp.searchEntities(CORPUS, q, null, 25);
  } catch (e) {
    $("search-msg").textContent = "search failed: " + emsg(e);
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
    const top = document.createElement("div");
    top.textContent =
      h.canonical_name + (h.entity_type ? "  ·  " + h.entity_type : "") + "  ·  " + h.degree + " links";
    b.appendChild(top);
    const desc = descOf(h);
    if (desc) {
      const d = document.createElement("div");
      d.className = "mdesc";
      d.textContent = desc;
      b.appendChild(d);
    }
    b.addEventListener("click", () => loadEntity(h.id));
    box.appendChild(b);
  }
}

// ─── Drill-down ──────────────────────────────────────────────────────
async function loadEntity(id) {
  let node;
  try {
    node = await window.meshApp.node(CORPUS, id);
  } catch (e) {
    $("search-msg").textContent = "load failed: " + emsg(e);
    return;
  }
  $("detail").hidden = false;
  $("d-type").textContent = node.entity_type;
  $("d-name").textContent = node.canonical_name;
  const attrs = node.attributes || {};
  $("d-desc").textContent =
    typeof attrs.description === "string" && attrs.description ? "“" + attrs.description + "”" : "";

  const rb = $("d-recon");
  rb.replaceChildren();
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
    rb.appendChild(box);
  }
  if (node.aliases && node.aliases.length) {
    const al = document.createElement("div");
    al.className = "alias-list";
    al.textContent =
      "Also seen as: " + node.aliases.slice(0, 12).join("  ·  ") +
      (node.aliases.length > 12 ? "  · …" : "");
    rb.appendChild(al);
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
  for (const e of node.edges || []) box.appendChild(renderEdge(e));
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
  if (e.source_chunk) {
    const conf =
      typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "";
    d.appendChild(makeEmailExpander(e.source_chunk, "the source email", conf));
  } else {
    const prov = document.createElement("div");
    prov.className = "prov";
    prov.textContent =
      "no source chunk" +
      (typeof e.confidence === "number" ? " · confidence " + e.confidence.toFixed(2) : "");
    d.appendChild(prov);
  }
  return d;
}

// A collapsed "read the source email" expander — the whole email behind a
// chunk id. Reused by edges and the timeline.
function makeEmailExpander(chunkId, labelText, suffixText) {
  const wrap = document.createElement("div");
  const line = document.createElement("div");
  line.className = "prov";
  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "link";
  const label = (open) =>
    (open ? "▾ hide " : "▸ read ") + labelText + " (chunk " + chunkId + ")";
  toggle.textContent = label(false);
  line.appendChild(toggle);
  if (suffixText) line.appendChild(document.createTextNode(suffixText));
  const full = document.createElement("pre");
  full.className = "card-full";
  full.hidden = true;
  let loaded = false;
  toggle.addEventListener("click", async () => {
    if (!loaded) {
      toggle.textContent = "loading email " + chunkId + "…";
      try {
        const ch = await window.meshApp.readChunk(CORPUS, String(chunkId));
        full.textContent = ch && ch.content ? ch.content : "(email is empty)";
      } catch (err) {
        full.textContent = "could not load email: " + emsg(err);
      }
      loaded = true;
      full.hidden = false;
    } else {
      full.hidden = !full.hidden;
    }
    toggle.textContent = label(!full.hidden);
  });
  wrap.appendChild(line);
  wrap.appendChild(full);
  return wrap;
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();

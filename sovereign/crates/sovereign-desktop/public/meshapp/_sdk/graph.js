// MeshApp SDK — a CSP-safe force-directed node-link graph.
//
// Pure vanilla JS + SVG (no d3, no imports) so it runs under the strict mesh-
// app CSP. A small velocity-Verlet simulation: all-pairs repulsion + spring
// attraction along edges + a gentle centering pull, settled over a fixed frame
// budget. Deterministic golden-angle init (no Math.random) → the same layout
// every load. Drag a node (re-heats the sim); a click without drag drills in.
//
// Takes `{ nodes, edges }` as returned by `meshapp_subgraph`:
//   nodes: [{ id, canonical_name, entity_type, degree, attributes? }]
//   edges: [{ source, target, relationship_type }]

import { svg, el, clear } from "./dom.js";
import { describe as descOf } from "./bridge.js";

const TYPE_COLOR = {
  person: "var(--person)",
  institution: "var(--inst)",
};

export function forceGraph(container, data, opts = {}) {
  const onNodeClick = opts.onNodeClick || (() => {});
  const colorOf = opts.color || ((n) => TYPE_COLOR[n.entity_type] || "var(--other)");
  const descOfNode = opts.describe || descOf;
  clear(container);

  const W = 760, H = 440;
  const root = svg("svg", { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "xMidYMid meet" });
  container.appendChild(root);
  const tip = el("div", { class: "map-tip" });
  container.appendChild(tip);

  const nodes = (data.nodes || []).map((n) => ({ ...n, x: 0, y: 0, vx: 0, vy: 0 }));
  if (!nodes.length) return;
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const links = (data.edges || [])
    .map((e) => ({ s: byId.get(e.source), t: byId.get(e.target) }))
    .filter((l) => l.s && l.t);

  const cx = W / 2, cy = H / 2;
  const GA = Math.PI * (3 - Math.sqrt(5));
  nodes.forEach((n, i) => {
    const r = 14 + Math.sqrt(i) * 26;
    n.x = cx + r * Math.cos(i * GA);
    n.y = cy + r * Math.sin(i * GA);
  });

  const maxDeg = nodes.reduce((m, n) => Math.max(m, n.degree || 1), 1);
  const radius = (n) => 5 + 14 * Math.sqrt((n.degree || 1) / maxDeg);

  const linkEls = links.map(() => {
    const ln = svg("line", { stroke: "#3a2f5c", "stroke-width": "1", "stroke-opacity": "0.8" });
    root.appendChild(ln);
    return ln;
  });

  let dragging = null, downAt = null, moved = false;
  for (const n of nodes) {
    const g = svg("g");
    g.appendChild(svg("circle", { r: radius(n), fill: colorOf(n), stroke: "#0e0b15", "stroke-width": "1.5" }));
    if (radius(n) >= 9) {
      const nm = n.canonical_name || n.id;
      const t = svg("text", { class: "nlabel", "text-anchor": "middle" });
      t.textContent = nm.length > 22 ? nm.slice(0, 21) + "…" : nm;
      g.appendChild(t);
      n._label = t;
    }
    const tipFor = (ev) => {
      clear(tip);
      tip.appendChild(el("div", { text: n.canonical_name + "  ·  " + (n.degree || 0) + " links" }));
      const d = descOfNode(n);
      if (d) tip.appendChild(el("div", { class: "td", text: d }));
      const r = container.getBoundingClientRect();
      tip.style.left = Math.min(r.width - 200, ev.clientX - r.left + 12) + "px";
      tip.style.top = Math.max(4, ev.clientY - r.top - 8) + "px";
      tip.style.opacity = "1";
    };
    g.addEventListener("mouseenter", tipFor);
    g.addEventListener("mousemove", tipFor);
    g.addEventListener("mouseleave", () => { tip.style.opacity = "0"; });
    g.addEventListener("pointerdown", (ev) => {
      dragging = n; downAt = { x: ev.clientX, y: ev.clientY }; moved = false;
      reheat();
      if (g.setPointerCapture) g.setPointerCapture(ev.pointerId);
    });
    g.addEventListener("pointermove", (ev) => {
      if (dragging !== n || !downAt) return;
      if (Math.hypot(ev.clientX - downAt.x, ev.clientY - downAt.y) > 4) moved = true;
      const r = root.getBoundingClientRect();
      n.x = ((ev.clientX - r.left) / r.width) * W;
      n.y = ((ev.clientY - r.top) / r.height) * H;
      paint();
    });
    g.addEventListener("pointerup", () => {
      if (dragging === n && !moved) onNodeClick(n.id);
      dragging = null; downAt = null;
    });
    root.appendChild(g);
    n._g = g;
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

  function physics() {
    for (let i = 0; i < nodes.length; i++) {
      const a = nodes[i];
      for (let j = i + 1; j < nodes.length; j++) {
        const b = nodes[j];
        const dx = a.x - b.x, dy = a.y - b.y;
        const d2 = dx * dx + dy * dy || 0.01;
        const d = Math.sqrt(d2);
        const f = 1600 / d2;
        const ux = dx / d, uy = dy / d;
        a.vx += ux * f; a.vy += uy * f; b.vx -= ux * f; b.vy -= uy * f;
      }
    }
    for (const l of links) {
      const dx = l.t.x - l.s.x, dy = l.t.y - l.s.y;
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
    physics();
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

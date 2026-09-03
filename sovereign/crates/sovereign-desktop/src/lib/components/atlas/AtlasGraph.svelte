<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Force-directed "epistemic landscape" map for a corpus's atom atlas.
  // Ported from the mesh-app SDK's proven, dependency-free graph engine
  // (public/meshapp/_sdk/graph.js): a velocity-Verlet simulation (all-pairs
  // repulsion + spring attraction + centering pull), deterministic golden-
  // angle init, settled over a fixed frame budget. Adapted to Svelte
  // lifecycle (rAF cancelled on destroy) and to atlas data: color by atom
  // type, size by salience, Tension edges drawn as bold red "fault lines"
  // carrying their crux on hover.
  import { onDestroy } from "svelte";
  import { atomTypeColor } from "./atomKinds";


  interface AtlasNode {
    id: string;
    label: string;
    atom_type: string;
    salience?: number;
    degree: number;
  }
  interface AtlasEdge {
    source: string;
    target: string;
    edge_type: string;
    crux?: string;
  }
  interface Props {
    nodes: AtlasNode[];
    edges: AtlasEdge[];
    onNodeClick?: (id: string) => void;
  }
  let { nodes, edges, onNodeClick = () => {} }: Props = $props();

  type SimNode = AtlasNode & {
    x: number;
    y: number;
    vx: number;
    vy: number;
    _g?: SVGGElement;
    _label?: SVGTextElement;
  };
  type SimLink = { s: SimNode; t: SimNode; tension: boolean; crux?: string };

  let container = $state<HTMLDivElement | null>(null);
  let rafId = 0;
  let destroyed = false;

  const colorFor = atomTypeColor;


  function svgEl(tag: string, attrs: Record<string, string | number> = {}): SVGElement {
    const n = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
    return n;
  }

  onDestroy(() => {
    destroyed = true;
    if (rafId) cancelAnimationFrame(rafId);
  });

  // Rebuild whenever the data (or the container binding) changes.
  $effect(() => {
    // touch deps
    nodes;
    edges;
    if (container) build(container);
  });

  function build(host: HTMLDivElement) {
    host.innerHTML = "";
    if (rafId) cancelAnimationFrame(rafId);

    const W = 760;
    const H = 440;
    const root = svgEl("svg", {
      viewBox: `0 0 ${W} ${H}`,
      preserveAspectRatio: "xMidYMid meet",
    });
    host.appendChild(root);
    const tip = document.createElement("div");
    tip.className = "atlas-graph-tip";
    host.appendChild(tip);

    const ns: SimNode[] = nodes.map((n) => ({ ...n, x: 0, y: 0, vx: 0, vy: 0 }));
    if (!ns.length) return;
    const byId = new Map<string, SimNode>(ns.map((n) => [n.id, n]));
    const links: SimLink[] = [];
    for (const e of edges) {
      const s = byId.get(e.source);
      const t = byId.get(e.target);
      if (s && t) links.push({ s, t, tension: e.edge_type === "Tension", crux: e.crux });
    }

    const cx = W / 2;
    const cy = H / 2;
    const GA = Math.PI * (3 - Math.sqrt(5));
    ns.forEach((n, i) => {
      const r = 14 + Math.sqrt(i) * 26;
      n.x = cx + r * Math.cos(i * GA);
      n.y = cy + r * Math.sin(i * GA);
    });

    const maxSal = ns.reduce((m, n) => Math.max(m, n.salience ?? 0), 0);
    const maxDeg = ns.reduce((m, n) => Math.max(m, n.degree || 1), 1);
    const radius = (n: SimNode): number =>
      maxSal > 0 && (n.salience ?? 0) > 0
        ? 5 + 13 * Math.sqrt((n.salience ?? 0) / maxSal)
        : 5 + 11 * Math.sqrt((n.degree || 1) / maxDeg);

    const linkEls: SVGElement[] = links.map((l) => {
      const ln = svgEl(
        "line",
        l.tension
          ? { stroke: "var(--error, #d4483a)", "stroke-width": "2", "stroke-opacity": "0.85" }
          : { stroke: "#3a2f5c", "stroke-width": "1", "stroke-opacity": "0.55" },
      );
      root.appendChild(ln);
      return ln;
    });

    let dragging: SimNode | null = null;
    let downAt: { x: number; y: number } | null = null;
    let moved = false;

    function showTip(n: SimNode, ev: PointerEvent | MouseEvent) {
      tip.innerHTML = "";
      const head = document.createElement("div");
      head.textContent = `${n.label}  ·  ${n.atom_type}`;
      tip.appendChild(head);
      const t = links.find((l) => l.tension && l.crux && (l.s.id === n.id || l.t.id === n.id));
      if (t?.crux) {
        const c = document.createElement("div");
        c.className = "td";
        c.textContent = `⚡ ${t.crux}`;
        tip.appendChild(c);
      }
      const r = host.getBoundingClientRect();
      tip.style.left = Math.min(r.width - 220, ev.clientX - r.left + 12) + "px";
      tip.style.top = Math.max(4, ev.clientY - r.top - 8) + "px";
      tip.style.opacity = "1";
    }

    for (const n of ns) {
      const g = svgEl("g") as SVGGElement;
      g.appendChild(
        svgEl("circle", {
          r: radius(n),
          fill: colorFor(n.atom_type),
          stroke: "#0e0b15",
          "stroke-width": "1.5",
        }),
      );
      if (radius(n) >= 8) {
        const nm = n.label || n.id;
        const t = svgEl("text", { class: "atlas-nlabel", "text-anchor": "middle" }) as SVGTextElement;
        t.textContent = nm.length > 22 ? nm.slice(0, 21) + "…" : nm;
        g.appendChild(t);
        n._label = t;
      }
      g.addEventListener("mouseenter", (ev) => showTip(n, ev));
      g.addEventListener("mousemove", (ev) => showTip(n, ev));
      g.addEventListener("mouseleave", () => {
        tip.style.opacity = "0";
      });
      g.addEventListener("pointerdown", (ev) => {
        dragging = n;
        downAt = { x: ev.clientX, y: ev.clientY };
        moved = false;
        reheat();
        g.setPointerCapture?.(ev.pointerId);
      });
      g.addEventListener("pointermove", (ev) => {
        if (dragging !== n || !downAt) return;
        if (Math.hypot(ev.clientX - downAt.x, ev.clientY - downAt.y) > 4) moved = true;
        const rr = root.getBoundingClientRect();
        n.x = ((ev.clientX - rr.left) / rr.width) * W;
        n.y = ((ev.clientY - rr.top) / rr.height) * H;
        paint();
      });
      g.addEventListener("pointerup", () => {
        if (dragging === n && !moved) onNodeClick(n.id);
        dragging = null;
        downAt = null;
      });
      root.appendChild(g);
      n._g = g;
    }

    function paint() {
      links.forEach((l, i) => {
        linkEls[i].setAttribute("x1", String(l.s.x));
        linkEls[i].setAttribute("y1", String(l.s.y));
        linkEls[i].setAttribute("x2", String(l.t.x));
        linkEls[i].setAttribute("y2", String(l.t.y));
      });
      for (const n of ns) {
        n._g?.setAttribute("transform", `translate(${n.x.toFixed(1)},${n.y.toFixed(1)})`);
        n._label?.setAttribute("y", String(-(radius(n) + 3)));
      }
    }

    function physics() {
      for (let i = 0; i < ns.length; i++) {
        const a = ns[i];
        for (let j = i + 1; j < ns.length; j++) {
          const b = ns[j];
          const dx = a.x - b.x;
          const dy = a.y - b.y;
          const d2 = dx * dx + dy * dy || 0.01;
          const d = Math.sqrt(d2);
          const f = 1600 / d2;
          const ux = dx / d;
          const uy = dy / d;
          a.vx += ux * f;
          a.vy += uy * f;
          b.vx -= ux * f;
          b.vy -= uy * f;
        }
      }
      for (const l of links) {
        const dx = l.t.x - l.s.x;
        const dy = l.t.y - l.s.y;
        const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
        const f = (d - 72) * 0.02;
        const ux = dx / d;
        const uy = dy / d;
        l.s.vx += ux * f;
        l.s.vy += uy * f;
        l.t.vx -= ux * f;
        l.t.vy -= uy * f;
      }
      for (const n of ns) {
        if (n === dragging) {
          n.vx = 0;
          n.vy = 0;
          continue;
        }
        n.vx += (cx - n.x) * 0.003;
        n.vy += (cy - n.y) * 0.003;
        n.vx *= 0.86;
        n.vy *= 0.86;
        n.x += n.vx;
        n.y += n.vy;
        n.x = Math.max(16, Math.min(W - 16, n.x));
        n.y = Math.max(16, Math.min(H - 16, n.y));
      }
      paint();
    }

    let frames = 0;
    let running = false;
    function loop() {
      if (destroyed) {
        running = false;
        return;
      }
      running = true;
      physics();
      frames++;
      if (frames < 300 || dragging) {
        rafId = requestAnimationFrame(loop);
      } else {
        running = false;
      }
    }
    function reheat() {
      frames = 0;
      if (!running) rafId = requestAnimationFrame(loop);
    }
    rafId = requestAnimationFrame(loop);
  }
</script>

<div class="atlas-graph" bind:this={container}></div>

<style>
  .atlas-graph {
    position: relative;
    width: 100%;
  }
  .atlas-graph :global(svg) {
    width: 100%;
    height: auto;
    display: block;
  }
  .atlas-graph :global(.atlas-nlabel) {
    font-size: 8px;
    fill: var(--text-muted);
    font-family: var(--font-sans);
    pointer-events: none;
  }
  .atlas-graph :global(.atlas-graph-tip) {
    position: absolute;
    pointer-events: none;
    opacity: 0;
    transition: opacity 0.1s;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 6px 9px;
    font-size: 0.72rem;
    color: var(--text-primary);
    max-width: 220px;
    z-index: 5;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
  }
  .atlas-graph :global(.atlas-graph-tip .td) {
    color: var(--text-muted);
    margin-top: 3px;
    font-size: 0.68rem;
    line-height: 1.35;
  }
</style>

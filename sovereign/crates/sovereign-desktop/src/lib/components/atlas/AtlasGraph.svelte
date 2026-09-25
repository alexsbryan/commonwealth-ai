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
  import { atomTypeColor, declaredTypeColor } from "./atomKinds";

  /** Settled layouts, keyed by `layoutKey` (the corpus id). Clicking a
   *  node swaps the map for the atom detail and Back remounts this
   *  component — with no memory the whole graph re-sorted from a fresh
   *  golden-angle init on every return ("clicking the map makes it a
   *  medley of chaos"). One module-level map means the view CHILLS: a
   *  remount restores exactly where every node and the reader's zoom/pan
   *  were, and a fully-restored graph does not run the sim at all. New
   *  nodes (a changed subgraph) still get a normal relaxation. */
  type LayoutMemo = {
    nodes: Map<string, { x: number; y: number }>;
    view?: { k: number; x: number; y: number };
  };
  const layoutMemory = new Map<string, LayoutMemo>();


  interface AtlasNode {
    id: string;
    label: string;
    atom_type: string;
    salience?: number;
    degree: number;
    /** The declared type's own noun (`hoard`, `mint`) on a declared
     *  corpus — absent otherwise. Present ⇒ the node colours by it
     *  (stage 0b: the custom atlas coloured by declared type). */
    declared_type?: string;
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
    /** Atom ids to highlight — the evidence path of one answer, fed
     *  from the walk ledger. Highlighted nodes keep their colour and
     *  gain a halo ring; everything else dims. No core or wire change:
     *  the caller already holds the ids. */
    highlight?: Set<string>;
    /** Stable identity for the layout memory (the corpus id). Absent =
     *  no memory: every mount flows from scratch, the old behaviour. */
    layoutKey?: string;
  }
  let {
    nodes,
    edges,
    onNodeClick = () => {},
    highlight = new Set<string>(),
    layoutKey,
  }: Props = $props();

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

  const colorFor = (n: AtlasNode): string =>
    n.declared_type ? declaredTypeColor(n.declared_type) : atomTypeColor(n.atom_type);


  function svgEl(tag: string, attrs: Record<string, string | number> = {}): SVGElement {
    const n = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
    return n;
  }

  let unmountSnapshot: (() => void) | null = null;
  onDestroy(() => {
    unmountSnapshot?.();
    destroyed = true;
    if (rafId) cancelAnimationFrame(rafId);
  });

  // Rebuild whenever the data (or the container binding) changes.
  $effect(() => {
    // touch deps
    nodes;
    edges;
    if (container) unmountSnapshot = build(container) ?? null;
  });

  function build(host: HTMLDivElement) {
    host.innerHTML = "";
    if (rafId) cancelAnimationFrame(rafId);

    // The virtual canvas scales with the node count, and the force
    // balance is tuned to keep a sparse graph INSIDE it. Measured on the
    // composed literary atlas (280 nodes, 386 edges, 23% isolates): the
    // fixed 760x440 canvas left the force equilibrium at r~400px against
    // a 220px half-height, so 85% of nodes sat in the four corner bands
    // once the box's aspect ratio cut the ring. A larger canvas alone does
    // not move the equilibrium; a weaker, range-capped repulsion plus a
    // stronger centering does, and the scale gives it room (corners 85% ->
    // 2%, isolates mean r 417 -> 301, nothing pinned). k = sqrt(N/60)
    // keeps small graphs byte-identical (k clamps at 1 below ~60 nodes).
    const K = Math.max(1, Math.sqrt(nodes.length / 60));
    const W = 760 * K;
    const H = 440 * K;
    const root = svgEl("svg", {
      viewBox: `0 0 ${W} ${H}`,
      preserveAspectRatio: "xMidYMid meet",
    });
    // The viewport transform — wheel-zoom about the cursor, drag-to-pan on
    // the background, double-click to reset. Applied to ONE group so the
    // sim's coordinates stay untouched; the map's own scale (K) already
    // sets the fitted size, and this is the reader's magnifier on top.
    const viewport = svgEl("g") as SVGGElement;
    let view = { k: 1, x: 0, y: 0 };
    const applyView = () =>
      viewport.setAttribute(
        "transform",
        `translate(${view.x.toFixed(2)} ${view.y.toFixed(2)}) scale(${view.k.toFixed(4)})`,
      );
    root.appendChild(viewport);
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
    // Layout memory: restore settled positions for known atoms; only NEW
    // atoms get the deterministic golden-angle seed. `settled` means every
    // node was restored — the caller then skips the sim entirely, which is
    // the "chill out" half.
    const memo: LayoutMemo | null = layoutKey
      ? (layoutMemory.get(layoutKey) ?? (() => {
          const m: LayoutMemo = { nodes: new Map() };
          layoutMemory.set(layoutKey, m);
          return m;
        })())
      : null;
    let restored = 0;
    ns.forEach((n, i) => {
      const p = memo?.nodes.get(n.id);
      if (p) {
        n.x = p.x;
        n.y = p.y;
        restored++;
        return;
      }
      const r = 14 + Math.sqrt(i) * 26;
      n.x = cx + r * Math.cos(i * GA);
      n.y = cy + r * Math.sin(i * GA);
    });
    const settled = ns.length > 0 && restored === ns.length;
    const snapshot = () => {
      if (!memo) return;
      for (const n of ns) memo.nodes.set(n.id, { x: n.x, y: n.y });
      memo.view = { ...view };
    };
    if (memo?.view) {
      view = { ...memo.view };
      applyView();
    }

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
      viewport.appendChild(ln);
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
      const lit = highlight.size === 0 || highlight.has(n.id);
      // The highlight set (an answer's evidence path) keeps its nodes at
      // full colour with a halo ring and dims the rest — the map-shot
      // beat is "the path this answer used, lit across the atlas".
      if (highlight.has(n.id)) {
        g.appendChild(
          svgEl("circle", {
            r: radius(n) + 5,
            fill: "none",
            stroke: colorFor(n),
            "stroke-width": "2.5",
            opacity: "0.85",
          }),
        );
      }
      g.appendChild(
        svgEl("circle", {
          r: radius(n),
          fill: colorFor(n),
          stroke: "#0e0b15",
          "stroke-width": "1.5",
          opacity: lit ? "1" : "0.25",
        }),
      );
      if (radius(n) >= 8) {
        const nm = n.label || n.id;
        const t = svgEl("text", { class: "atlas-nlabel", "text-anchor": "middle" }) as SVGTextElement;
        t.textContent = nm.length > 22 ? nm.slice(0, 21) + "…" : nm;
        if (!lit) t.setAttribute("opacity", "0.3");
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
        // NO reheat. A drag is a LOCAL act — the reader separating a
        // clustered node to see it — and reheating made the whole graph
        // re-settle around the pointer ("all the reflow"). The dragged
        // node follows the pointer via paint(); nothing else moves.
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
        // The dropped position is the reader's arrangement: remember it
        // (the settle-snapshot may never run again once the layout is
        // frozen).
        snapshot();
      });
      viewport.appendChild(g);
      n._g = g;
    }

    // Wheel-zoom about the cursor: the standard screen->world correction so
    // the point under the pointer stays put. Clamped so a stray trackpad
    // flick cannot lose the map.
    root.addEventListener(
      "wheel",
      (ev: WheelEvent) => {
        ev.preventDefault();
        const rr = root.getBoundingClientRect();
        const sx = ((ev.clientX - rr.left) / rr.width) * W;
        const sy = ((ev.clientY - rr.top) / rr.height) * H;
        const factor = Math.exp(-ev.deltaY * 0.0015);
        const k = Math.min(6, Math.max(0.25, view.k * factor));
        const scale = k / view.k;
        view.x = sx - (sx - view.x) * scale;
        view.y = sy - (sy - view.y) * scale;
        view.k = k;
        applyView();
      },
      { passive: false },
    );
    // Drag-pan on the BACKGROUND only — a node's own pointerdown captures
    // first, so dragging a node still moves that node.
    let panFrom: { x: number; y: number; vx: number; vy: number } | null = null;
    root.addEventListener("pointerdown", (ev: PointerEvent) => {
      if (ev.target !== root && ev.target !== viewport) return;
      const rr = root.getBoundingClientRect();
      panFrom = {
        x: ((ev.clientX - rr.left) / rr.width) * W,
        y: ((ev.clientY - rr.top) / rr.height) * H,
        vx: view.x,
        vy: view.y,
      };
      root.setPointerCapture?.(ev.pointerId);
    });
    root.addEventListener("pointermove", (ev: PointerEvent) => {
      if (!panFrom) return;
      const rr = root.getBoundingClientRect();
      const sx = ((ev.clientX - rr.left) / rr.width) * W;
      const sy = ((ev.clientY - rr.top) / rr.height) * H;
      view.x = panFrom.vx + (sx - panFrom.x);
      view.y = panFrom.vy + (sy - panFrom.y);
      applyView();
    });
    root.addEventListener("pointerup", () => {
      panFrom = null;
    });
    // Double-click resets the reader's view; the fitted layout is the home.
    root.addEventListener("dblclick", () => {
      view = { k: 1, x: 0, y: 0 };
      applyView();
    });

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
          // Range-capped repulsion: beyond ~200px the outward push only
          // inflates the ring the centering then has to fight.
          if (d2 > 200 * 200) continue;
          const d = Math.sqrt(d2);
          const f = 600 / d2;
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
        const f = (d - 48) * 0.02;
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
        n.vx += (cx - n.x) * 0.02;
        n.vy += (cy - n.y) * 0.02;
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
      if (frames < 300) {
        rafId = requestAnimationFrame(loop);
      } else {
        running = false;
        snapshot();
      }
    }
    if (settled) {
      // Every node's position is remembered: paint the frozen layout, no
      // sim, no reflow — and a drag stays local on top of it.
      paint();
    } else {
      rafId = requestAnimationFrame(loop);
    }
    // The unmount half of the snapshot: the click that swaps this view for
    // the atom detail destroys the component before the loop exhausts, and
    // THAT is the state the user comes back to.
    return snapshot;
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

// MeshApp SDK — composable views.
//
// Each renders into a container with the shared classes from meshapp.css and
// takes data in + callbacks out (the app owns fetching via the bridge). They're
// the pieces an explorer composes: a scale banner, a type toggle, a search box,
// an on-ramp thread list, a degree/hotspot bar list, a timeline, and the
// reconciliation list with its reveal.

import { el, clear, emsg, fmtInt, friendlySignal } from "./dom.js";
import { describe } from "./bridge.js";

/** Scale/provenance banner. `items`: `{num, cap, glow?}` objects, or "→". */
export function scaleBanner(container, items) {
  clear(container);
  for (const it of items) {
    if (it === "→" || it === "arrow") {
      container.appendChild(el("div", { class: "arrow", text: "→" }));
      continue;
    }
    container.appendChild(el("div", { class: "stat" },
      el("div", { class: "num" + (it.glow ? " glow" : ""), text: String(it.num) }),
      el("div", { class: "cap", text: it.cap })));
  }
}

/** Pill toggle. `options`: `{type, label}`; calls `onChange(type)`. */
export function typeToggle(container, options, opts = {}) {
  clear(container);
  const buttons = [];
  for (const o of options) {
    const b = el("button", {
      type: "button", "data-type": o.type, text: o.label,
      "aria-pressed": String(o.type === opts.initial),
    });
    b.addEventListener("click", () => {
      for (const x of buttons) x.setAttribute("aria-pressed", String(x === b));
      opts.onChange && opts.onChange(o.type);
    });
    buttons.push(b);
    container.appendChild(b);
  }
}

/**
 * Search box: input + button + a `#matches` list + `#search-msg` status, all
 * created inside `container`. Searches via `bridge.search`, renders each hit
 * with its degree + description, and calls `onPick(id)`.
 */
export function searchBox(container, bridge, opts = {}) {
  clear(container);
  const input = el("input", { type: "text", id: "q", placeholder: opts.placeholder || "Search", "aria-label": opts.ariaLabel || "Search" });
  const button = el("button", { type: "button", id: "search", text: "Search" });
  const matches = el("div", { class: "matches", id: "matches" });
  const msg = el("div", { class: "meta", id: "search-msg" });
  container.appendChild(el("div", { class: "search-row" }, input, button));
  container.appendChild(matches);
  container.appendChild(msg);

  const run = async () => {
    msg.textContent = "";
    clear(matches);
    const q = input.value.trim();
    if (!q) return;
    let hits;
    try {
      hits = await bridge.search(q, opts.nodeType || null, opts.limit || 25);
    } catch (e) {
      msg.textContent = "search failed: " + emsg(e);
      return;
    }
    if (!hits || !hits.length) {
      msg.textContent = "no match for '" + q + "'";
      return;
    }
    for (const h of hits) {
      const b = el("button", { type: "button", class: "match" },
        el("div", { text: h.canonical_name + (h.entity_type ? "  ·  " + h.entity_type : "") + "  ·  " + (h.degree || 0) + " links" }));
      const d = describe(h);
      if (d) b.appendChild(el("div", { class: "mdesc", text: d }));
      b.addEventListener("click", () => opts.onPick && opts.onPick(h.id));
      matches.appendChild(b);
    }
  };
  button.addEventListener("click", run);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") run(); });
}

/** On-ramp thread cards. `items`: entity-shaped `{id, canonical_name, entity_type, attributes}`. */
export function threadList(container, items, opts = {}) {
  clear(container);
  let any = false;
  for (const h of items) {
    if (!h) continue;
    any = true;
    const b = el("button", { type: "button", class: "thread" },
      el("div", {},
        el("span", { class: "tname", text: h.canonical_name }),
        el("span", { class: "tkind", text: h.entity_type || "" })));
    const d = describe(h);
    if (d) b.appendChild(el("div", { class: "tdesc", text: "“" + d + "”" }));
    b.addEventListener("click", () => opts.onPick && opts.onPick(h.id));
    container.appendChild(b);
  }
  if (!any) container.appendChild(el("div", { class: "meta", text: "(featured threads unavailable)" }));
}

/** Ranked bar list (UAP hotspots, degree ranking). `rows`: `{id, name, value}`. */
export function barList(container, rows, opts = {}) {
  clear(container);
  if (!rows.length) {
    container.appendChild(el("div", { class: "meta", text: opts.empty || "nothing to rank." }));
    return;
  }
  const max = rows.reduce((m, r) => Math.max(m, r.value), 1);
  for (const r of rows) {
    const left = el("div", {},
      el("div", { text: r.name }),
      el("div", { class: "bar", style: { width: Math.max(8, Math.round((r.value / max) * 100)) + "%" } }));
    const b = el("button", { type: "button", class: "hot" }, left,
      el("div", { class: "cnt", text: r.value + (opts.countSuffix || " links") }));
    b.addEventListener("click", () => opts.onPick && opts.onPick(r.id));
    container.appendChild(b);
  }
}

const MONTHS = ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/** `2001-11` → `Nov '01`. */
export function monthLabel(ym) {
  const [y, m] = String(ym).split("-");
  return (MONTHS[+m] || m) + " '" + (y || "").slice(2);
}

/** Monthly bar chart. `buckets`: `{ym, count, chunk_ids}`; calls `onMonth(bucket)`. */
export function timelineChart(container, buckets, opts = {}) {
  clear(container);
  if (!buckets.length) return;
  const max = buckets.reduce((m, b) => Math.max(m, b.count), 1);
  for (const b of buckets) {
    const col = el("button", {
      type: "button", class: "tl-col", "aria-pressed": "false",
      title: b.count + " in " + monthLabel(b.ym),
    },
      el("div", { class: "tl-track" },
        el("div", { class: "tl-bar", style: { height: Math.max(2, Math.round((b.count / max) * 100)) + "%" } })),
      el("div", { class: "tl-lab", text: monthLabel(b.ym) }));
    col.addEventListener("click", () => {
      for (const c of container.children) c.setAttribute("aria-pressed", "false");
      col.setAttribute("aria-pressed", "true");
      opts.onMonth && opts.onMonth(b);
    });
    container.appendChild(col);
  }
}

/**
 * Reconciliation list with the reveal: each row shows the canonical + the
 * signal that fired; clicking the row cascades its surface forms in then
 * collapses them to the canonical; clicking the canonical name calls
 * `onOpen(canonical_id)`.
 */
export function reconciliationList(container, merges, opts = {}) {
  clear(container);
  for (const m of merges) {
    const canon = el("div", { class: "canon", text: m.canonical_name, style: { cursor: "pointer" }, title: "open this entity" });
    canon.addEventListener("click", (e) => { e.stopPropagation(); opts.onOpen && opts.onOpen(m.canonical_id); });
    for (const s of m.signals_fired || []) {
      canon.appendChild(el("span", { class: "chip signal", style: { marginLeft: "8px" }, text: friendlySignal(s) }));
    }
    const reveal = el("div", { class: "reveal" });
    (m.surface_forms || []).forEach((sf, i) => {
      reveal.appendChild(el("span", { class: "sf", style: { transitionDelay: (i * 0.08).toFixed(2) + "s" }, text: "“" + sf + "”" }));
    });
    reveal.appendChild(el("span", { class: "to", text: " ⟶ " }));
    reveal.appendChild(el("span", { class: "canon2", text: m.canonical_name }));

    const row = el("div", { class: "merge" }, canon,
      el("div", { class: "forms", text: (m.surface_forms || []).join("  ·  ") + "  →  " + m.canonical_name }),
      reveal);
    let open = false;
    row.addEventListener("click", () => { open = !open; reveal.classList.toggle("on", open); });
    container.appendChild(row);
  }
}

export { fmtInt };

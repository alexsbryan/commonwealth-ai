// MeshApp SDK — CSP-safe DOM helpers.
//
// Mesh-app bundles run under a strict CSP (`script-src 'self'`, no inline
// scripts, no external network). These helpers build the DOM by construction —
// never `innerHTML` — so a bundle can't smuggle markup into the page, and the
// SDK stays a dependency-free ES module served from the same origin.

/** `document.getElementById`. */
export const $ = (id) => document.getElementById(id);

/** Remove all children of a node. */
export function clear(node) {
  if (node) node.replaceChildren();
}

/**
 * Build an element. Props: `class`/`text` are special; `onX` adds a listener;
 * `style` accepts an object; anything else is `setAttribute`. Children may be
 * strings (→ text nodes), nodes, arrays, or null (skipped). NEVER sets
 * innerHTML — text always goes through `textContent`/text nodes.
 */
export function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null) continue;
    if (k === "class") node.className = v;
    else if (k === "text") node.textContent = v;
    else if (k === "style" && typeof v === "object") Object.assign(node.style, v);
    else if (k.startsWith("on") && typeof v === "function") {
      node.addEventListener(k.slice(2).toLowerCase(), v);
    } else node.setAttribute(k, v);
  }
  append(node, children);
  return node;
}

/** Append children (strings → text nodes, arrays flattened, null skipped). */
export function append(node, children) {
  for (const c of children.flat ? children.flat(Infinity) : children) {
    if (c == null || c === false) continue;
    node.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
  }
  return node;
}

const SVG_NS = "http://www.w3.org/2000/svg";
/** Build an SVG element (attributes only — SVG has no `className` setter). */
export function svg(tag, props = {}) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null) continue;
    if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2).toLowerCase(), v);
    else node.setAttribute(k, v);
  }
  return node;
}

/** A short error string from a thrown value. */
export const emsg = (e) => (e && e.message ? e.message : String(e));

/** Comma-grouped integer, e.g. 3722 → "3,722". */
export const fmtInt = (n) => (Number(n) || 0).toLocaleString("en-US");

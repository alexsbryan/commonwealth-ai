// kit.mjs — a tiny hand-drawn diagram kit built on rough.js (headless generator).
// The excalidraw feel comes from three things: wobbly strokes, hachure fills,
// and one print-hand font. Everything below is in service of those three.
import rough from 'roughjs';
import { PATRICK_HAND_WOFF2_B64 } from './font.mjs';

const G = rough.generator();

// ---- palette: pencil-on-warm-paper. Each card carries its own paper so it
// reads the same whether GitHub shows it on a light or dark page. ----
export const C = {
  paper:   '#fbf7ef',
  paper2:  '#f3ecdd',   // faint secondary panel
  ink:     '#2f2b26',   // charcoal, never pure black
  inkSoft: '#6d675d',
  // four layers / roles, each a distinct pencil:
  user:    '#3b6fd4',   // surfaces / you
  sov:     '#e08a3c',   // sovereign — the assistant (amber/terracotta)
  mesh:    '#2f9e6f',   // cmnwlth — the mesh (green)
  corpus:  '#8a5cd0',   // corpus-engine — knowledge (purple)
  base:    '#7c8a97',   // oicp — the base
  // semantics of the gate:
  ok:      '#2f9e6f',
  warn:    '#d99327',
  stop:    '#cf5044',
  hi:      '#ffd75e',   // marker highlight
};

// tuneable per-color soft fill (hachure), kept low so text stays readable.
const FILL = { hachureGap: 7, fillWeight: 1.1, roughness: 1.35 };

// ---- rough.js opset -> SVG path serialization (no DOM) ----
function opsToPath(ops) {
  let d = '';
  for (const op of ops) {
    const p = op.data;
    if (op.op === 'move') d += `M${p[0].toFixed(2)} ${p[1].toFixed(2)} `;
    else if (op.op === 'bcurveTo')
      d += `C${p[0].toFixed(2)} ${p[1].toFixed(2)} ${p[2].toFixed(2)} ${p[3].toFixed(2)} ${p[4].toFixed(2)} ${p[5].toFixed(2)} `;
    else if (op.op === 'lineTo') d += `L${p[0].toFixed(2)} ${p[1].toFixed(2)} `;
  }
  return d.trim();
}
function drawableToSvg(drawable, { stroke, strokeWidth = 2, fill } = {}) {
  let out = '';
  for (const set of drawable.sets) {
    const d = opsToPath(set.ops);
    if (!d) continue;
    if (set.type === 'path')
      out += `<path d="${d}" fill="none" stroke="${stroke}" stroke-width="${strokeWidth}" stroke-linecap="round" stroke-linejoin="round"/>`;
    else if (set.type === 'fillPath')
      out += `<path d="${d}" fill="${fill}" stroke="none"/>`;
    else if (set.type === 'fillSketch')
      out += `<path d="${d}" fill="none" stroke="${fill}" stroke-width="${FILL.fillWeight}" stroke-linecap="round"/>`;
  }
  return out;
}

let SEED = 1;
const seed = () => (SEED = (SEED * 1103515245 + 12345) & 0x7fffffff);

// ---- primitives ----------------------------------------------------------

// wobbly rounded rectangle with optional hachure fill (approximated: rough.js
// has no rounded-rect, so we build a path with quadratic-ish corners).
export function box(x, y, w, h, {
  stroke = C.ink, fill = null, fillColor = null, wash = null, washOpacity = 0.13,
  sw = 2.2, r = 14, rough: ro = 1.3,
} = {}) {
  const path =
    `M${x + r} ${y} L${x + w - r} ${y} Q${x + w} ${y} ${x + w} ${y + r} ` +
    `L${x + w} ${y + h - r} Q${x + w} ${y + h} ${x + w - r} ${y + h} ` +
    `L${x + r} ${y + h} Q${x} ${y + h} ${x} ${y + h - r} ` +
    `L${x} ${y + r} Q${x} ${y} ${x + r} ${y} Z`;
  let s = '';
  // wash = a pale solid tint behind the box, drawn first, so text stays legible.
  if (wash) {
    const wd = G.path(path, { fill: wash, fillStyle: 'solid', stroke: 'none', roughness: ro, seed: seed() });
    s += `<g opacity="${washOpacity}">${drawableToSvg(wd, { stroke: 'none', strokeWidth: 0, fill: wash })}</g>`;
  }
  const opts = { stroke, strokeWidth: sw, roughness: ro, bowing: 1.4, seed: seed() };
  if (fill) { opts.fill = fill; opts.fillStyle = 'hachure'; opts.hachureGap = FILL.hachureGap; opts.hachureAngle = -41; opts.fillWeight = FILL.fillWeight; }
  const d = G.path(path, opts);
  s += drawableToSvg(d, { stroke, strokeWidth: sw, fill: fillColor || fill });
  return s;
}

// a database cylinder (for on-disk storage / indexes)
export function cylinder(x, y, w, h, { stroke = C.ink, fill = null, sw = 2.2 } = {}) {
  const ry = Math.min(14, h * 0.16);
  let s = '';
  const body = G.path(
    `M${x} ${y + ry} L${x} ${y + h - ry} Q${x + w / 2} ${y + h + ry} ${x + w} ${y + h - ry} L${x + w} ${y + ry}`,
    { stroke, strokeWidth: sw, roughness: 1.1, seed: seed(), ...(fill ? { fill, fillStyle: 'hachure', hachureGap: 8, fillWeight: 1, hachureAngle: -41 } : {}) });
  const top = G.ellipse(x + w / 2, y + ry, w, ry * 2, { stroke, strokeWidth: sw, roughness: 1.0, seed: seed(), fill: C.paper, fillStyle: 'solid' });
  s += drawableToSvg(body, { stroke, strokeWidth: sw, fill });
  s += drawableToSvg(top, { stroke, strokeWidth: sw, fill: C.paper });
  return s;
}

// a soft cloud/blob (network / the mesh)
export function blob(cx, cy, w, h, { stroke = C.mesh, fill = null, sw = 2.2, gap = 9 } = {}) {
  const d = G.ellipse(cx, cy, w, h, { stroke, strokeWidth: sw, roughness: 1.7, bowing: 2.2, seed: seed(),
    ...(fill ? { fill, fillStyle: 'hachure', hachureGap: gap, fillWeight: 1, hachureAngle: 20 } : {}) });
  return drawableToSvg(d, { stroke, strokeWidth: sw, fill });
}

// a woven mesh between points: strands across every pair + a couple of
// mid-span cross strands, with a node dot at each point. Reads as "a net of
// trusted machines," not three wires.
export function weave(points, { stroke = C.mesh, sw = 1.7, node = 5 } = {}) {
  let s = '';
  const mids = [];
  for (let i = 0; i < points.length; i++)
    for (let j = i + 1; j < points.length; j++) {
      const [x1, y1] = points[i], [x2, y2] = points[j];
      s += line(x1, y1, x2, y2, { stroke, sw, ro: 1.6 });
      mids.push([(x1 + x2) / 2, (y1 + y2) / 2]);
    }
  // secondary strands weaving the mid-points, so the interior looks meshed
  for (let i = 0; i < mids.length; i++)
    for (let j = i + 1; j < mids.length; j++)
      s += line(mids[i][0], mids[i][1], mids[j][0], mids[j][1], { stroke, sw: sw * 0.8, ro: 1.9 });
  for (const [x, y] of points) s += circle(x, y, node, { stroke, fill: stroke, sw: 1.6 });
  return s;
}

export function circle(cx, cy, d, { stroke = C.ink, fill = null, sw = 2.2 } = {}) {
  const dr = G.circle(cx, cy, d, { stroke, strokeWidth: sw, roughness: 1.3, seed: seed(),
    ...(fill ? { fill, fillStyle: 'hachure', hachureGap: 6, fillWeight: 1, hachureAngle: -30 } : {}) });
  return drawableToSvg(dr, { stroke, strokeWidth: sw, fill });
}

export function line(x1, y1, x2, y2, { stroke = C.ink, sw = 2, dash = null, ro = 1.2 } = {}) {
  const d = G.line(x1, y1, x2, y2, { stroke, strokeWidth: sw, roughness: ro, bowing: 1.5, seed: seed() });
  let s = drawableToSvg(d, { stroke, strokeWidth: sw });
  if (dash) s = s.replace(/<path /g, `<path stroke-dasharray="${dash}" `);
  return s;
}

// an arrow: wobbly shaft + two-stroke hand arrowhead. dir in radians optional.
export function arrow(x1, y1, x2, y2, { stroke = C.ink, sw = 2.2, dash = null, head = 13, ro = 1.1 } = {}) {
  let s = line(x1, y1, x2, y2, { stroke, sw, dash, ro });
  const ang = Math.atan2(y2 - y1, x2 - x1);
  const a1 = ang + Math.PI - 0.42, a2 = ang + Math.PI + 0.42;
  s += drawableToSvg(G.line(x2, y2, x2 + head * Math.cos(a1), y2 + head * Math.sin(a1), { stroke, strokeWidth: sw, roughness: 0.9, seed: seed() }), { stroke, strokeWidth: sw });
  s += drawableToSvg(G.line(x2, y2, x2 + head * Math.cos(a2), y2 + head * Math.sin(a2), { stroke, strokeWidth: sw, roughness: 0.9, seed: seed() }), { stroke, strokeWidth: sw });
  return s;
}

// hand-drawn underline (for titles / emphasis)
export function underline(x1, x2, y, { stroke = C.sov, sw = 3.2 } = {}) {
  return line(x1, y, x2, y, { stroke, sw, ro: 1.8 });
}

// a marker highlight rectangle (translucent), drawn UNDER text
export function highlight(x, y, w, h, { color = C.hi } = {}) {
  const d = G.rectangle(x, y, w, h, { fill: color, fillStyle: 'solid', stroke: 'none', roughness: 2, seed: seed() });
  return `<g opacity="0.5">${drawableToSvg(d, { stroke: 'none', strokeWidth: 0, fill: color })}</g>`;
}

// a little checkmark / cross stamp
export function stamp(cx, cy, kind = 'check', { color = C.ok, s = 12 } = {}) {
  if (kind === 'check')
    return `<path d="M${cx - s} ${cy} L${cx - s * 0.25} ${cy + s * 0.8} L${cx + s} ${cy - s * 0.9}" fill="none" stroke="${color}" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round"/>`;
  if (kind === 'cross')
    return `<path d="M${cx - s} ${cy - s} L${cx + s} ${cy + s} M${cx + s} ${cy - s} L${cx - s} ${cy + s}" fill="none" stroke="${color}" stroke-width="3.4" stroke-linecap="round"/>`;
  return '';
}

// ---- text (Patrick Hand). Manual wrap by char budget. ----
function esc(t) { return t.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;'); }
export function text(x, y, str, {
  size = 21, color = C.ink, anchor = 'middle', weight = 400, rotate = 0, opacity = 1, spacing = 0,
} = {}) {
  const tr = rotate ? ` transform="rotate(${rotate} ${x} ${y})"` : '';
  const ls = spacing ? ` letter-spacing="${spacing}"` : '';
  return `<text x="${x}" y="${y}" font-family="'Patrick Hand', cursive" font-size="${size}" fill="${color}" text-anchor="${anchor}" font-weight="${weight}" opacity="${opacity}"${ls}${tr}>${esc(str)}</text>`;
}
// wrapped multi-line text block, centered in a box of width w (char-budget wrap)
export function textBlock(cx, cy, str, { size = 20, color = C.ink, w = 180, lh = 1.18, anchor = 'middle' } = {}) {
  const budget = Math.max(6, Math.floor(w / (size * 0.5)));
  const words = str.split(' ');
  const lines = []; let cur = '';
  for (const wd of words) {
    if ((cur + ' ' + wd).trim().length <= budget) cur = (cur + ' ' + wd).trim();
    else { if (cur) lines.push(cur); cur = wd; }
  }
  if (cur) lines.push(cur);
  const total = (lines.length - 1) * size * lh;
  let s = '';
  lines.forEach((ln, i) => { s += text(cx, cy - total / 2 + i * size * lh + size * 0.34, ln, { size, color, anchor }); });
  return s;
}

// ---- document assembly ---------------------------------------------------
export function svg(W, H, body, { title = '' } = {}) {
  const font = `@font-face{font-family:'Patrick Hand';font-style:normal;font-weight:400;src:url(data:font/woff2;base64,${PATRICK_HAND_WOFF2_B64}) format('woff2');}`;
  // paper with a faint deckle border
  const paper =
    `<rect x="0" y="0" width="${W}" height="${H}" fill="${C.paper}"/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" font-family="'Patrick Hand', cursive" role="img"${title ? ` aria-label="${esc(title)}"` : ''}>
<style>${font} text{-webkit-font-smoothing:antialiased;}</style>
${title ? `<title>${esc(title)}</title>` : ''}
${paper}
${body}
</svg>`;
}

export const util = { seedReset: () => { SEED = 1; } };

import fs from 'fs';
import path from 'path';
import * as k from './kit.mjs';
const { C } = k;

const OUT = process.env.OUT || './out';
fs.mkdirSync(OUT, { recursive: true });
const write = (name, s) => { fs.writeFileSync(path.join(OUT, name), s); console.log('wrote', name, (s.length / 1024).toFixed(1), 'KB'); };

// ---- shared glyphs ----------------------------------------------------------
function laptop(x, y, w, accent) {
  const h = w * 0.62; let s = '';
  s += k.box(x, y, w, h, { stroke: C.ink, wash: accent, sw: 2.2, r: 8 });
  s += k.line(x - w * 0.12, y + h + 9, x + w + w * 0.12, y + h + 9, { stroke: C.ink, sw: 2.4 });
  s += k.line(x - w * 0.12, y + h + 9, x + w * 0.06, y + h, { stroke: C.ink, sw: 2.2 });
  s += k.line(x + w + w * 0.12, y + h + 9, x + w - w * 0.06, y + h, { stroke: C.ink, sw: 2.2 });
  return s;
}
function model(cx, cy, r, color) {
  let s = k.circle(cx, cy, r * 2, { stroke: color, fill: color, sw: 2.3 });
  for (const a of [-0.9, -0.2, 0.5, 1.2]) s += k.line(cx + Math.cos(a) * r, cy + Math.sin(a) * r, cx + Math.cos(a) * (r + 9), cy + Math.sin(a) * (r + 9), { stroke: color, sw: 1.8 });
  return s;
}
// a titled section header with a hand underline
function header(x, y, str, color, size = 40) {
  return k.text(x, y, str, { size, anchor: 'start', color: C.ink }) +
    k.underline(x + 2, x + str.length * size * 0.42, y + 16, { stroke: color, sw: 4 });
}
// a small "page N" tab, top-right, to make the flip-book feel
function pageTab(W, n, of, color) {
  return k.box(W - 96, 26, 70, 40, { stroke: color, sw: 2, r: 8 }) +
    k.text(W - 61, 53, `${n}/${of}`, { size: 20, color });
}
// a labelled pipeline pill
function pill(x, y, w, h, label, color, sub) {
  let s = k.box(x, y, w, h, { stroke: color, wash: color, sw: 2.3, r: 12 });
  s += k.textBlock(x + w / 2, y + h / 2 - (sub ? 8 : 0), label, { size: 19, w: w - 12 });
  if (sub) s += k.text(x + w / 2, y + h - 12, sub, { size: 14, color: C.inkSoft });
  return s;
}

// ============================================================================
// 00 — HERO
// ============================================================================
function hero() {
  const W = 1000, H = 560; let b = '';
  b += header(60, 74, 'Commonwealth AI', C.sov, 46);
  b += k.text(60, 118, 'an assistant that runs on your own computer —', { size: 24, anchor: 'start', color: C.inkSoft });
  b += k.text(60, 148, 'and across a few you trust, when one machine isn’t enough.', { size: 24, anchor: 'start', color: C.inkSoft });

  const lx = 70, ly = 210, lw = 380, lh = 300;
  b += k.box(lx, ly, lw, lh, { stroke: C.sov, sw: 2.6, r: 18 });
  b += k.text(lx + 20, ly - 12, 'svrnmesh — the assistant', { size: 24, anchor: 'start', color: C.sov });
  const mx = lx + 55, my = ly + 66;
  b += k.box(mx, my, 270, 150, { stroke: C.ink, wash: C.sov, sw: 2.2, r: 12 });
  b += k.text(mx + 135, my - 12, 'your machine', { size: 19, color: C.inkSoft });
  b += model(mx + 72, my + 58, 24, C.sov);
  b += k.text(mx + 72, my + 108, 'the model', { size: 18, color: C.ink });
  b += k.text(mx + 72, my + 128, 'that answers you', { size: 18, color: C.ink });
  b += k.cylinder(mx + 172, my + 28, 58, 66, { stroke: C.ink, fill: C.corpus, sw: 2 });
  b += k.text(mx + 201, my + 118, 'your notes,', { size: 16, color: C.ink });
  b += k.text(mx + 201, my + 136, 'memory, files', { size: 16, color: C.ink });
  b += k.stamp(lx + 44, ly + lh - 26, 'check', { color: C.ok, s: 11 });
  b += k.text(lx + lw / 2 + 16, ly + lh - 20, 'nothing leaves unless you ask', { size: 20, color: C.ok });

  const cxm = lx + lw + 44;
  b += k.text(cxm + 24, ly + 132, 'and, when', { size: 20, color: C.inkSoft });
  b += k.text(cxm + 24, ly + 156, 'you want…', { size: 20, color: C.inkSoft });
  b += k.arrow(cxm - 8, ly + 178, cxm + 62, ly + 178, { stroke: C.ink, sw: 2.6 });

  const rx = 618, ry = 210, rw = 322, rh = 300;
  b += k.box(rx, ry, rw, rh, { stroke: C.mesh, sw: 2.6, r: 18 });
  b += k.text(rx + 20, ry - 12, 'cmnwlth — the optional mesh', { size: 22, anchor: 'start', color: C.mesh });
  // four peer machines, woven directly to each other — no server in the middle
  const pts = [[rx + 76, ry + 96], [rx + 248, ry + 84], [rx + 252, ry + 196], [rx + 84, ry + 206]];
  b += k.weave(pts, { stroke: C.mesh, sw: 1.8, node: 5 });
  for (const [px, py] of pts) b += laptop(px - 23, py - 15, 46, C.paper2);
  b += k.text(rx + rw / 2, ry + rh - 44, 'a few machines you trust, joined by invitation', { size: 17, color: C.ink });
  b += k.text(rx + rw / 2, ry + rh - 22, 'answering as one — nothing leaves the group', { size: 16, color: C.mesh });

  // the soul line: what makes it different
  b += k.text(W / 2, H - 18, 'no account · no subscription · no company in the middle — just the code, on your hardware', { size: 17, color: C.inkSoft });
  write('00-hero.svg', k.svg(W, H, b, { title: 'Commonwealth AI — an assistant that runs on your own computer' }));
}

// ============================================================================
// 01 — THE TERRITORY: four projects, one dependency direction
// ============================================================================
function territory() {
  const W = 1040, H = 600; let b = '';
  b += header(60, 74, 'The pieces, and what each is for', C.user, 38);
  b += k.text(60, 108, 'Not a stack to memorize — the few parts you’ll meet, and the job each one does for you.', { size: 20, anchor: 'start', color: C.inkSoft });
  b += pageTab(W, 1, 5, C.user);

  // Sovereign — the thing you actually talk to (the heart, on your machine)
  const sx = 60, sy = 156, sw = 360, sh = 150;
  b += k.box(sx, sy, sw, sh, { stroke: C.sov, wash: C.sov, sw: 2.8, r: 16 });
  b += k.text(sx + 22, sy + 40, 'Sovereign', { size: 30, anchor: 'start', color: C.sov });
  b += k.text(sx + 22, sy + 72, 'the assistant you talk to.', { size: 19, anchor: 'start', color: C.ink });
  b += k.text(sx + 22, sy + 98, 'CLI · desktop · server — the', { size: 19, anchor: 'start', color: C.ink });
  b += k.text(sx + 22, sy + 122, 'whole loop, on your machine.', { size: 19, anchor: 'start', color: C.ink });

  // the two things it composes
  b += k.text(sx + sw + 46, sy - 8, 'it composes two things:', { size: 17, anchor: 'start', color: C.inkSoft });
  const ex = sx + sw + 46, ew = 474;
  // a local model (inference)
  b += k.box(ex, sy + 6, ew, 62, { stroke: C.ink, wash: C.sov, sw: 2.2, r: 12 });
  b += k.text(ex + 18, sy + 32, 'a local model', { size: 20, anchor: 'start', color: C.ink });
  b += k.text(ex + 18, sy + 54, 'runs on your hardware — answers you, no cloud', { size: 15, anchor: 'start', color: C.inkSoft });
  // corpus-engine (knowledge)
  b += k.box(ex, sy + 82, ew, 84, { stroke: C.ink, wash: C.corpus, sw: 2.2, r: 12 });
  b += k.text(ex + 18, sy + 110, 'corpus-engine', { size: 20, anchor: 'start', color: C.corpus });
  b += k.text(ex + 18, sy + 133, 'turns your sources — notes, email, Wikipedia —', { size: 15, anchor: 'start', color: C.ink });
  b += k.text(ex + 18, sy + 154, 'into knowledge it can search and cite', { size: 15, anchor: 'start', color: C.ink });
  // arrows Sovereign -> each
  b += k.arrow(sx + sw + 4, sy + 42, ex - 4, sy + 37, { stroke: C.ink, sw: 2 });
  b += k.arrow(sx + sw + 4, sy + 96, ex - 4, sy + 116, { stroke: C.ink, sw: 2 });

  // cmnwlth — optional, wraps the above
  const my = 366, mw = 900;
  b += k.box(60, my, mw, 120, { stroke: C.mesh, wash: C.mesh, sw: 2.8, r: 16 });
  b += k.text(84, my + 40, 'cmnwlth', { size: 28, anchor: 'start', color: C.mesh });
  b += k.box(84, my + 52, 96, 26, { stroke: C.mesh, sw: 1.8, r: 8 });
  b += k.text(132, my + 71, 'optional', { size: 15, color: C.mesh });
  b += k.text(300, my + 40, 'pool machines with people you trust — a research group, a clinic,', { size: 18, anchor: 'start', color: C.ink });
  b += k.text(300, my + 66, 'a co-op. Run a model bigger than any one of you could,', { size: 18, anchor: 'start', color: C.ink });
  b += k.text(300, my + 92, 'or share knowledge that never leaves its owner’s disk.', { size: 18, anchor: 'start', color: C.ink });
  // it federates the group above
  b += k.arrow(200, my - 2, 240, sy + sh + 8, { stroke: C.mesh, sw: 2, dash: '3 6', head: 11 });
  b += k.text(232, my - 12, 'federates both, by invitation', { size: 15, anchor: 'start', color: C.mesh });

  // OICP — demoted to an honest footnote, NOT a foundation
  b += k.text(60, my + 152, 'It speaks the ordinary OpenAI API, so any OpenAI-compatible tool just works — OICP only adds a thin layer on top,', { size: 16, anchor: 'start', color: C.inkSoft });
  b += k.text(60, my + 174, 'so nodes can advertise what they’re good at. No server in the middle, no account. The real foundation is the one', { size: 16, anchor: 'start', color: C.inkSoft });
  b += k.text(60, my + 196, 'thing the others rest on: you can read the source and check every claim yourself.', { size: 16, anchor: 'start', color: C.inkSoft });
  write('01-territory.svg', k.svg(W, H, b, { title: 'The pieces, and what each is for' }));
}

// ============================================================================
// 02 — ONE MESSAGE'S JOURNEY: nothing ships unverified
// ============================================================================
function journey() {
  const W = 1040, H = 620; let b = '';
  b += header(60, 74, 'One message’s journey', C.sov, 40);
  b += k.text(60, 108, 'Every question rides the same path — and the answer is held until a verifier checks it.', { size: 20, anchor: 'start', color: C.inkSoft });
  b += pageTab(W, 2, 5, C.sov);

  const rowY = 160;
  // stages left→right
  b += k.box(60, rowY, 120, 70, { stroke: C.user, wash: C.user, sw: 2.3, r: 12 });
  b += k.textBlock(120, rowY + 35, 'your message', { size: 18, w: 108 });
  b += k.arrow(184, rowY + 35, 214, rowY + 35, { stroke: C.ink, sw: 2.2 });

  b += pill(218, rowY, 130, 70, 'Router — what kind of ask?', C.sov);
  b += k.arrow(352, rowY + 35, 382, rowY + 35, { stroke: C.ink, sw: 2.2 });

  b += pill(386, rowY, 150, 70, 'Retrieval — search ALL your sources', C.corpus);
  b += k.arrow(540, rowY + 35, 570, rowY + 35, { stroke: C.ink, sw: 2.2 });

  b += pill(574, rowY, 130, 70, 'Synthesis — draft an answer', C.sov);
  b += k.arrow(708, rowY + 35, 740, rowY + 35, { stroke: C.ink, sw: 2.2 });

  // annotation on retrieval
  b += k.text(461, rowY + 96, 'local corpora ∥ mesh peers ∥ your docs', { size: 14, color: C.inkSoft });

  // THE GATE — big central element
  const gx = 744, gy = rowY - 4, gw = 234, gh = 78;
  b += k.box(gx, gy, gw, gh, { stroke: C.ink, wash: C.warn, sw: 3, r: 14 });
  b += k.text(gx + gw / 2, gy + 30, 'the grounding gate', { size: 22, color: C.ink });
  b += k.text(gx + gw / 2, gy + 56, 'hold · extract claims · verify', { size: 16, color: C.inkSoft });

  // gate detail box
  const dy = 300;
  b += k.box(300, dy, 440, 150, { stroke: C.warn, wash: C.warn, sw: 2.4, r: 14 });
  b += k.text(520, dy + 30, 'each claim, checked against the sealed evidence', { size: 19, color: C.ink });
  b += k.text(340, dy + 66, '“free will is compatible with…”', { size: 17, anchor: 'start', color: C.inkSoft });
  b += k.stamp(710, dy + 60, 'check', { color: C.ok, s: 10 });
  b += k.text(340, dy + 96, '“…and the figure was $2.3M”', { size: 17, anchor: 'start', color: C.inkSoft });
  b += k.stamp(710, dy + 90, 'check', { color: C.ok, s: 10 });
  b += k.text(520, dy + 132, 'the model never originates a number', { size: 16, color: C.stop });
  // gate → detail connector
  b += k.arrow(gx + gw / 2, gy + gh, 520, dy, { stroke: C.ink, sw: 2, dash: '3 6' });

  // three outcomes
  const oy = 500;
  b += k.box(80, oy, 260, 88, { stroke: C.ok, wash: C.ok, sw: 2.4, r: 12 });
  b += k.stamp(112, oy + 30, 'check', { color: C.ok, s: 11 });
  b += k.text(210, oy + 30, 'released', { size: 21, color: C.ok });
  b += k.text(210, oy + 60, 'answer with [Source: …] citations', { size: 15, color: C.ink });

  b += k.box(390, oy, 260, 88, { stroke: C.warn, wash: C.warn, sw: 2.4, r: 12 });
  b += k.text(520, oy + 30, 'rewrite', { size: 21, color: C.warn });
  b += k.text(520, oy + 60, 'fix the unsupported bits, re-check', { size: 15, color: C.ink });

  b += k.box(700, oy, 260, 88, { stroke: C.stop, wash: C.stop, sw: 2.4, r: 12 });
  b += k.text(830, oy + 30, 'abstain', { size: 21, color: C.stop });
  b += k.text(830, oy + 60, 'honest “not in my sources”', { size: 15, color: C.ink });

  b += k.arrow(500, dy + 150, 210, oy, { stroke: C.ok, sw: 2, dash: '3 6' });
  b += k.arrow(520, dy + 150, 520, oy, { stroke: C.warn, sw: 2, dash: '3 6' });
  b += k.arrow(540, dy + 150, 830, oy, { stroke: C.stop, sw: 2, dash: '3 6' });
  // rewrite loops back
  b += k.text(520, oy - 8, 'loops back to the gate ↺', { size: 14, color: C.inkSoft });
  write('02-journey.svg', k.svg(W, H, b, { title: "One message's journey — nothing ships unverified" }));
}

// ============================================================================
// 03 — A RECIPE IS THE UNIT OF KNOWLEDGE
// ============================================================================
function recipe() {
  const W = 1160, H = 560; let b = '';
  b += header(60, 74, 'A recipe is the unit of knowledge', C.corpus, 38);
  b += k.text(60, 108, 'Wikipedia, the Stanford Encyclopedia, your email — every corpus enters through one declarative pipeline.', { size: 19, anchor: 'start', color: C.inkSoft });
  b += pageTab(W, 3, 5, C.corpus);

  // TOML card
  b += k.box(60, 150, 210, 220, { stroke: C.ink, wash: C.corpus, sw: 2.4, r: 12 });
  b += k.text(84, 180, 'recipe.toml', { size: 22, anchor: 'start', color: C.corpus });
  const toml = ['[corpus]', 'id = "sep"', '[acquire]', 'type = "bulk"', '[extract]', 'type = "xml"', '[chunk]', 'type = "paragraph"'];
  toml.forEach((l, i) => b += k.text(84, 212 + i * 20, l, { size: 15, anchor: 'start', color: C.ink }));
  b += k.arrow(274, 260, 306, 260, { stroke: C.ink, sw: 2.4 });

  // pipeline stages
  const stages = ['acquire', 'extract', 'filter', 'chunk', 'embed', 'index'];
  const px = 312, pw = 104, gap = 8, py = 220;
  stages.forEach((sname, i) => {
    const x = px + i * (pw + gap);
    b += pill(x, py, pw, 80, sname, C.corpus);
    if (i < stages.length - 1) b += k.arrow(x + pw, py + 40, x + pw + gap, py + 40, { stroke: C.ink, sw: 2 });
  });
  // landing cylinder
  const lastX = px + stages.length * (pw + gap);
  b += k.arrow(lastX - 2, py + 40, lastX + 26, py + 40, { stroke: C.ink, sw: 2.2 });
  b += k.cylinder(lastX + 30, py + 6, 84, 74, { stroke: C.ink, fill: C.corpus, sw: 2 });
  b += k.text(lastX + 72, py + 104, '~/.sovereign/', { size: 14, color: C.inkSoft });
  b += k.text(lastX + 72, py + 122, 'indexes/', { size: 14, color: C.inkSoft });

  // optional enrich, dashed
  b += k.arrow(px + 2 * (pw + gap) + pw / 2, py + 80, px + 2 * (pw + gap) + pw / 2, py + 132, { stroke: C.inkSoft, sw: 2, dash: '3 6' });
  b += k.box(px + (pw + gap), py + 138, 320, 62, { stroke: C.inkSoft, sw: 2, r: 12 });
  b += k.text(px + (pw + gap) + 160, py + 165, 'optional: enrich into an atlas of', { size: 16, color: C.inkSoft });
  b += k.text(px + (pw + gap) + 160, py + 187, 'typed atoms — claims, entities, tensions', { size: 16, color: C.inkSoft });

  // custody flags
  const fy = 400;
  b += k.text(60, fy + 4, 'Two flags carry the custody policy:', { size: 18, anchor: 'start', color: C.ink });
  b += k.box(60, fy + 20, 300, 56, { stroke: C.mesh, wash: C.mesh, sw: 2.2, r: 10 });
  b += k.text(76, fy + 46, 'query_sharing', { size: 18, anchor: 'start', color: C.mesh });
  b += k.text(76, fy + 66, 'may peers search it & get cited snippets?', { size: 13, anchor: 'start', color: C.ink });
  b += k.box(378, fy + 20, 300, 56, { stroke: C.sov, wash: C.sov, sw: 2.2, r: 10 });
  b += k.text(394, fy + 46, 'mesh_sharing', { size: 18, anchor: 'start', color: C.sov });
  b += k.text(394, fy + 66, 'may the index bytes replicate to peers?', { size: 13, anchor: 'start', color: C.ink });
  b += k.box(696, fy + 20, 280, 56, { stroke: C.stop, wash: C.stop, sw: 2.2, r: 10 });
  b += k.text(712, fy + 46, 'scope = "local"', { size: 18, anchor: 'start', color: C.stop });
  b += k.text(712, fy + 66, 'keep it off the mesh entirely', { size: 13, anchor: 'start', color: C.ink });
  write('03-recipe.svg', k.svg(W, H, b, { title: 'A recipe is the unit of knowledge' }));
}

// ============================================================================
// 04 — THE MESH: chunks travel, corpora don't
// ============================================================================
function meshCustody() {
  const W = 1040, H = 580; let b = '';
  b += header(60, 74, 'Chunks travel, corpora don’t', C.mesh, 40);
  b += k.text(60, 108, 'A cross-corpus query, custody preserved end to end. Here the corpus stays on one machine — because it’s set that way.', { size: 19, anchor: 'start', color: C.inkSoft });
  b += pageTab(W, 4, 5, C.mesh);

  // node B (asks)
  const bx = 70, by = 180, nw = 340, nh = 300;
  b += k.box(bx, by, nw, nh, { stroke: C.user, sw: 2.6, r: 16 });
  b += k.text(bx + 20, by - 12, 'you — asking (host nothing)', { size: 20, anchor: 'start', color: C.user });
  b += laptop(bx + 40, by + 40, 70, C.user);
  b += k.text(bx + 76, by + 130, 'embeds your question', { size: 16, color: C.ink });
  b += k.text(bx + 76, by + 150, 'no local match', { size: 16, color: C.inkSoft });
  b += k.box(bx + 30, by + 180, 280, 96, { stroke: C.ink, wash: C.sov, sw: 2, r: 10 });
  b += k.text(bx + 170, by + 206, 'merge → synthesize → grounding gate', { size: 15, color: C.ink });
  b += k.stamp(bx + 56, by + 240, 'check', { color: C.ok, s: 10 });
  b += k.text(bx + 180, by + 244, 'answer cites [Source: sep]', { size: 16, color: C.ok });
  b += k.text(bx + 180, by + 264, 'served by your peer', { size: 14, color: C.inkSoft });

  // node A (hosts)
  const ax = 630;
  b += k.box(ax, by, nw, nh, { stroke: C.mesh, sw: 2.6, r: 16 });
  b += k.text(ax + 20, by - 12, 'a peer you trust — hosts the corpus', { size: 20, anchor: 'start', color: C.mesh });
  b += k.cylinder(ax + 40, by + 46, 90, 90, { stroke: C.ink, fill: C.corpus, sw: 2.2 });
  b += k.text(ax + 85, by + 168, 'the sep index', { size: 16, color: C.ink });
  b += k.text(ax + 210, by + 80, 'query_sharing = true', { size: 15, anchor: 'start', color: C.mesh });
  b += k.text(ax + 210, by + 104, 'searches allowed', { size: 14, anchor: 'start', color: C.inkSoft });
  b += k.text(ax + 210, by + 148, 'mesh_sharing = false', { size: 15, anchor: 'start', color: C.stop });
  b += k.text(ax + 210, by + 172, 'bytes never copied', { size: 14, anchor: 'start', color: C.inkSoft });
  b += k.box(ax + 30, by + 205, 280, 70, { stroke: C.ink, wash: C.mesh, sw: 2, r: 10 });
  b += k.text(ax + 170, by + 232, 'ledger: served your query', { size: 15, color: C.ink });
  b += k.text(ax + 170, by + 256, '(credit recorded, no balance)', { size: 13, color: C.inkSoft });

  // arrows between
  const midY = by + 90;
  b += k.arrow(bx + nw + 6, midY, ax - 6, midY, { stroke: C.ink, sw: 2.4 });
  b += k.text((bx + nw + ax) / 2, midY - 12, 'search', { size: 16, color: C.ink });
  b += k.arrow(ax - 6, midY + 60, bx + nw + 6, midY + 60, { stroke: C.ok, sw: 2.6 });
  b += k.text((bx + nw + ax) / 2, midY + 50, 'scored chunks', { size: 16, color: C.ok });
  b += k.text((bx + nw + ax) / 2, midY + 92, '+ provenance', { size: 14, color: C.inkSoft });

  // the blocked bytes
  b += k.line((bx + nw + ax) / 2 - 44, midY + 120, (bx + nw + ax) / 2 + 44, midY + 120, { stroke: C.stop, sw: 2.4, dash: '4 6' });
  b += k.stamp((bx + nw + ax) / 2, midY + 120, 'cross', { color: C.stop, s: 11 });
  b += k.text((bx + nw + ax) / 2, midY + 158, 'the index bytes', { size: 15, color: C.stop });
  b += k.text((bx + nw + ax) / 2, midY + 178, 'refused here', { size: 14, color: C.inkSoft });

  // custody is a choice, not a wall — corpora CAN replicate if you allow it
  b += k.box(60, 500, 920, 62, { stroke: C.mesh, wash: C.mesh, sw: 2, r: 12 });
  b += k.text(W / 2, 524, 'Custody is your choice: this corpus keeps  mesh_sharing = false,  so its bytes never leave.', { size: 17, color: C.ink });
  b += k.text(W / 2, 548, 'Set it  true  and the same corpus can live on several machines you trust — replicated on purpose, never behind your back.', { size: 16, color: C.mesh });
  write('04-mesh-custody.svg', k.svg(W, H, b, { title: "Chunks travel, corpora don't" }));
}

// ============================================================================
// 05 — EVERY LAYER HAS A GATE
// ============================================================================
function gates() {
  const W = 1000, H = 660; let b = '';
  b += header(60, 74, 'You don’t have to trust us — verify', C.ok, 36);
  b += k.text(60, 108, 'The claims are properties you can check, not promises. Every one is gated — in the runtime, on the bench, in CI — and the checks run in the open.', { size: 18, anchor: 'start', color: C.inkSoft });
  b += pageTab(W, 5, 5, C.ok);

  const rows = [
    ['Grounding gate', 'runtime', 'every grounded answer is verified before release', C.sov],
    ['Numeric audit', 'runtime', 'every figure is value-matched to tool output', C.sov],
    ['Chaos monkey', 'bench', 'honesty under adversarial questioning — two red lines', C.corpus],
    ['Mechanism fidelity', 'bench', 'does the model reason, or pattern-match?', C.corpus],
    ['docs-gate', 'CI', 'every claim in the docs must resolve against the code', C.user],
    ['arch-gate', 'CI', 'architectural debt is ratcheted, never grows', C.user],
  ];
  const y0 = 156, rh = 66, gap = 8;
  rows.forEach((r, i) => {
    const y = y0 + i * (rh + gap);
    b += k.box(60, y, 880, rh, { stroke: r[3], wash: r[3], sw: 2.2, r: 12 });
    b += k.stamp(96, y + rh / 2, 'check', { color: C.ok, s: 12 });
    b += k.text(130, y + 30, r[0], { size: 22, anchor: 'start', color: C.ink });
    // scope tag
    b += k.box(130, y + 40, 60, 20, { stroke: r[3], sw: 1.6, r: 6 });
    b += k.text(160, y + 55, r[1], { size: 13, color: r[3] });
    b += k.text(210, y + 46, r[2], { size: 17, anchor: 'start', color: C.ink });
  });
  b += k.text(W / 2, H - 22, 'measured, not asserted — read them, run them', { size: 22, color: C.ok });
  write('05-gates.svg', k.svg(W, H, b, { title: 'You don’t have to trust us — verify' }));
}

// ============================================================================
// 06 — RUN A MODEL BIGGER THAN YOUR MACHINE
// ============================================================================
function biggerModel() {
  const W = 1020, H = 560; let b = '';
  b += header(60, 74, 'Run a model bigger than your machine', C.mesh, 36);
  b += k.text(60, 108, 'Pool a few machines you trust. The model’s layers spread across them; you talk to it as if it were local.', { size: 19, anchor: 'start', color: C.inkSoft });

  // the big model as a stack of layers, sliced
  b += k.text(150, 168, 'one big model', { size: 20, color: C.ink });
  b += k.text(150, 190, '(too big for one box)', { size: 15, color: C.inkSoft });
  const layers = 12, lx = 70, lyy = 210, lw = 160, layH = 20;
  for (let i = 0; i < layers; i++) {
    const col = i < 5 ? C.sov : i < 9 ? C.mesh : C.corpus;
    b += k.box(lx, lyy + i * layH, lw, layH - 3, { stroke: C.ink, wash: col, sw: 1.4, r: 4 });
  }
  // slice brackets
  b += k.text(lx + lw + 26, lyy + 52, 'host’s share', { size: 15, anchor: 'start', color: C.sov });
  b += k.text(lx + lw + 26, lyy + 150, 'worker 1', { size: 15, anchor: 'start', color: C.mesh });
  b += k.text(lx + lw + 26, lyy + 218, 'worker 2', { size: 15, anchor: 'start', color: C.corpus });

  b += k.arrow(lx + lw + 120, lyy + 130, lx + lw + 190, lyy + 130, { stroke: C.ink, sw: 2.4 });

  // three machines
  const machines = [['host', C.sov, 'holds the file · splits · serves'], ['worker 1', C.mesh, 'lends memory + GPU'], ['worker 2', C.corpus, 'lends memory + GPU']];
  const bx = 470, bw = 260, bh = 96, bgap = 18;
  machines.forEach((m, i) => {
    const y = 200 + i * (bh + bgap);
    b += k.box(bx, y, bw, bh, { stroke: m[1], wash: m[1], sw: 2.4, r: 12 });
    b += laptop(bx + 26, y + 22, 52, m[1]);
    b += k.text(bx + 175, y + 34, m[0], { size: 20, color: m[1] });
    b += k.text(bx + 175, y + 62, m[2], { size: 14, color: C.ink });
    if (i > 0) b += k.arrow(bx + 90, 200 + (i - 1) * (bh + bgap) + bh, bx + 90, y, { stroke: C.inkSoft, sw: 1.8, dash: '3 6' });
  });
  b += k.text(bx + bw / 2, 200 - 12, 'you talk to the host', { size: 15, color: C.inkSoft });

  // annotation
  b += k.box(760, 220, 220, 200, { stroke: C.inkSoft, sw: 2, r: 12 });
  b += k.text(870, 250, 'once loaded:', { size: 18, color: C.ink });
  b += k.text(870, 288, 'each worker keeps', { size: 15, color: C.ink });
  b += k.text(870, 308, 'its slice resident', { size: 15, color: C.ink });
  b += k.text(870, 346, 'per answer, only a', { size: 15, color: C.ink });
  b += k.text(870, 366, 'few KB of state', { size: 15, color: C.mesh });
  b += k.text(870, 386, 'cross the wire', { size: 15, color: C.ink });
  b += k.text(W / 2, H - 20, 'three 64 GB machines can hold a model no one of them could', { size: 18, color: C.mesh });
  write('06-bigger-model.svg', k.svg(W, H, b, { title: 'Run a model bigger than your machine' }));
}

// ============================================================================
// 07 — A WORKFLOW: describe the shape, run the file
// ============================================================================
function workflow() {
  const W = 1020, H = 560; let b = '';
  b += header(60, 74, 'A workflow', C.sov, 40);
  b += k.text(60, 108, 'Read a folder, run a model over each file, save the result — written as one small text file. No code.', { size: 19, anchor: 'start', color: C.inkSoft });

  // source
  b += k.box(70, 180, 150, 110, { stroke: C.corpus, wash: C.corpus, sw: 2.4, r: 12 });
  b += k.text(145, 210, 'source', { size: 22, color: C.corpus });
  b += k.text(145, 240, 'a folder of', { size: 16, color: C.ink });
  b += k.text(145, 262, 'notes', { size: 16, color: C.ink });
  b += k.text(145, 306, '{item.text} →', { size: 15, color: C.inkSoft });
  b += k.arrow(222, 235, 268, 235, { stroke: C.ink, sw: 2.2 });

  // step 1: model
  b += k.box(272, 180, 210, 110, { stroke: C.sov, wash: C.sov, sw: 2.4, r: 12 });
  b += k.text(377, 208, 'step: summary', { size: 20, color: C.sov });
  b += k.text(377, 236, 'uses = model:thoughtful', { size: 15, color: C.ink });
  b += k.text(377, 262, 'prompt: “summarize', { size: 15, color: C.ink });
  b += k.text(377, 282, 'this note…”', { size: 15, color: C.ink });
  b += k.arrow(484, 235, 530, 235, { stroke: C.ink, sw: 2.2 });

  // step 2: save
  b += k.box(534, 180, 210, 110, { stroke: C.mesh, wash: C.mesh, sw: 2.4, r: 12 });
  b += k.text(639, 208, 'step: save', { size: 20, color: C.mesh });
  b += k.text(639, 236, 'uses = tool:write_file', { size: 15, color: C.ink });
  b += k.text(639, 262, 'content =', { size: 15, color: C.ink });
  b += k.text(639, 282, '{summary.output}', { size: 15, color: C.sov });

  // the reference arrow (curved annotation)
  b += k.arrow(639, 296, 470, 296, { stroke: C.sov, sw: 2, dash: '3 6' });
  b += k.text(555, 320, 'mentioning a result IS the arrow —', { size: 16, color: C.sov });
  b += k.text(555, 342, 'you never draw the wiring yourself', { size: 16, color: C.inkSoft });

  // output
  b += k.arrow(746, 235, 792, 235, { stroke: C.ink, sw: 2.2 });
  b += k.cylinder(796, 195, 84, 76, { stroke: C.ink, fill: C.mesh, sw: 2 });
  b += k.text(838, 296, 'summaries/', { size: 15, color: C.inkSoft });

  // the living version
  b += k.box(70, 400, 880, 110, { stroke: C.ink, wash: C.warn, sw: 2.4, r: 14 });
  b += k.text(110, 434, 'make it living:', { size: 22, anchor: 'start', color: C.warn });
  b += k.text(110, 466, 'svrn corpus watch ~/meetings --on-change meeting-to-done', { size: 18, anchor: 'start', color: C.ink });
  b += k.text(110, 494, 'point the daemon at a folder — from then on, each new file runs the workflow itself, unattended.', { size: 16, anchor: 'start', color: C.inkSoft });
  write('07-workflow.svg', k.svg(W, H, b, { title: 'A workflow — describe the shape, run the file' }));
}

hero(); territory(); journey(); recipe(); meshCustody(); gates(); biggerModel(); workflow();
console.log('done');

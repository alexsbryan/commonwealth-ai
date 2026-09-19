// SPDX-License-Identifier: AGPL-3.0-or-later
// The phone in the room: nothing installed, no daemon, no mesh key. It reads
// the QR off the wall, opens the page the door serves, types a name, edits the
// doc and asks the room — and every one of those goes through the SAME
// `window.ring` the product ships, fetched from the door as a browser fetches
// it (`guest_door::ring_shim`), never a reimplementation of it here.
//
// What IS this instrument's own is the browser around that shim: a `location`
// carrying the link's fragment, and a `fetch` that resolves the shim's
// relative paths against the page's origin. That is the same stand-in as
// ring-doc-demo.sh's in-container forwarder — "the browser on that machine" —
// and it is deliberately the only thing supplied, so a bug in the shim shows
// up here rather than being papered over.
//
// Everything it "typed" is reported, so the census counts what a person would
// actually have typed and nothing else. Reads env, writes ONE json object.
//
//   QR_SVG   the wall's QR, as bytes off the wall (the camera)
//   REPO     the repo root, for the ring-doc page's own adapter
//   NAME     the name to type, once
//   EDITS    how many further edits to make (default 1)
//   ASK      a question, or empty for none
//   COLLIDE  a roster member's name to try FIRST, expecting a refusal
//   LABEL    what to call this phone in the output
import { readFileSync } from "node:fs";

import { decodeSvgQr, gridFromSvg, quietZone } from "./qr.mjs";

const { QR_SVG, REPO, NAME, EDITS = "1", ASK = "", COLLIDE = "", LABEL = "phone" } = process.env;
const out = { label: LABEL, typed: [], errors: [], acts: [] };
const done = (fatal) => {
  if (fatal) out.fatal = String(fatal.message || fatal);
  console.log(JSON.stringify(out));
  process.exit(0);
};
const now = () => Date.now() / 1000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try {
  // ── the scan ──────────────────────────────────────────────────────────────
  const svg = readFileSync(QR_SVG, "utf8");
  out.qr = { bytes: svg.length, quiet_zone: quietZone(gridFromSvg(svg)) };
  const link = decodeSvgQr(svg);
  out.link = link;
  const hash = link.indexOf("#");
  if (hash < 0) throw new Error("the scanned link carries no fragment, so it carries no bearer");
  const base = link.slice(0, hash);
  const params = new URLSearchParams(link.slice(hash + 1));
  const token = params.get("token");
  if (!token) throw new Error("the link's fragment names no token");
  out.expires_at = Number(params.get("exp")) || null;
  out.summary = params.get("s");
  out.token_in_query = base.includes("token");
  const origin = new URL(base).origin;
  out.origin = origin;

  // ── the page the door serves ──────────────────────────────────────────────
  const page = await fetch(base);
  out.page = { status: page.status };
  const html = await page.text();
  out.page.has_shim = html.includes('<script src="/ring/__ring.js">');
  if (!page.ok || !out.page.has_shim) throw new Error(`the door served no ring page (${page.status})`);

  // ── the browser around the product's shim ─────────────────────────────────
  const shimResp = await fetch(`${origin}/ring/__ring.js`);
  const shim = await shimResp.text();
  out.shim = { status: shimResp.status, bytes: shim.length };
  if (!shimResp.ok) throw new Error(`the door served no shim (${shimResp.status})`);
  const realFetch = globalThis.fetch;
  const abs = (u) => (String(u).startsWith("http") ? String(u) : origin + String(u));
  globalThis.fetch = (u, o) => realFetch(abs(u), { ...(o || {}), signal: AbortSignal.timeout(300000) });
  globalThis.window = {};
  globalThis.location = { hash: `#${link.slice(hash + 1)}` };
  new Function(shim)();
  const ring = globalThis.window.ring;
  if (!ring) throw new Error("the door's shim defined no window.ring");

  // ── the doc, as the page holds it ─────────────────────────────────────────
  const A = await import(`file://${REPO}/sovereign/apps/ring-doc/adapter.js`);
  const { Y } = await import(`file://${REPO}/sovereign/apps/ring-doc/vendor/ring-doc-bundle.js`);
  const fold = (log, reducer, init) => {
    let acc = init;
    for (const op of (log && log.ops) || []) {
      if (op.voided || op.payload == null) continue;
      acc = reducer(acc, op.payload, op);
    }
    return acc;
  };
  const ydoc = new Y.Doc();
  const applied = new Set();
  const frag = ydoc.getXmlFragment(A.PROSE_FRAGMENT);
  const log = await ring.log();
  out.namespace = log.namespace || null;
  out.roster = Object.keys((log.roster || {}).members || {});
  out.acts_before = A.decodeActs(log, fold).acts.length;
  A.applyNew(ydoc, A.decodeActs(log, fold).acts, applied);

  let update = null;
  ydoc.on("update", (u) => {
    update = u;
  });
  /// One edit, recorded through the shim, under `who`. Returns the act.
  ///
  /// The paragraph is created here when the doc is empty, which is what the
  /// editor does for the first person to type into a fresh ring — the wall
  /// shows a blank page until somebody writes, and in this room the first
  /// somebody is a guest. It goes in the SAME transaction as the text so one
  /// act carries both; two would put a paragraph on every screen that nobody
  /// had typed into.
  const write = async (word, who) => {
    update = null;
    ydoc.transact(() => {
      if (frag.length === 0) {
        const para = new Y.XmlElement("paragraph");
        para.insert(0, [new Y.XmlText()]);
        frag.insert(0, [para]);
      }
      const text = frag.get(0).get(0);
      text.insert(text.length, ` ${word}`);
    });
    if (!update) throw new Error("the edit produced no update to record");
    return ring.record(A.changeAct(Y.mergeUpdates([update]), A.DOC_ID, who));
  };

  // ── the refusal, before anything else: a guest may not wear a member's name
  if (COLLIDE) {
    out.typed.push({ leg: "collide", string: COLLIDE, note: "a roster member's name, tried on purpose" });
    try {
      const act = await write("collided", COLLIDE);
      out.collision = { refused: false, act: act && act.id };
    } catch (e) {
      out.collision = { refused: true, error: String(e.message || e) };
    }
  }

  // ── the name, typed once ──────────────────────────────────────────────────
  out.typed.push({ leg: "wall", string: NAME, note: "the name, typed on the phone" });
  const t_name = now();
  const named = await write("here", NAME);
  out.acts.push({ leg: "wall", id: named.id, actor: named.actor, at: t_name, took_s: +(now() - t_name).toFixed(2) });

  // ── the edits that follow ─────────────────────────────────────────────────
  for (let i = 0; i < Number(EDITS); i += 1) {
    const word = `edit${i}`;
    out.typed.push({ leg: "edit", string: word, note: "a word typed into the doc" });
    const t = now();
    const act = await write(word, NAME);
    out.acts.push({ leg: "edit", id: act.id, actor: act.actor, at: t, took_s: +(now() - t).toFixed(2) });
  }

  // ── the ask ───────────────────────────────────────────────────────────────
  if (ASK) {
    out.typed.push({ leg: "ask", string: ASK, note: "the question, typed on the phone" });
    const t = now();
    try {
      const { answer, epistemic_state } = await ring.ask(ASK);
      out.ask = {
        at: t,
        answered_s: +(now() - t).toFixed(2),
        answer,
        // Rendered by the page's own code, so what is judged is what a person
        // on the phone would have read under the answer.
        sources: A.citationLines(epistemic_state),
        epistemic_state,
      };
    } catch (e) {
      out.ask = { at: t, answered_s: +(now() - t).toFixed(2), error: String(e.message || e) };
    }
  }

  // ── what this bearer must NOT reach ───────────────────────────────────────
  // Raw, not through the shim: the shim offers no way to ask for these, and
  // the point is what the DOOR refuses, not what the page declines to send.
  const probe = async (name, path, init) => {
    try {
      const r = await realFetch(origin + path, {
        ...(init || {}),
        headers: { authorization: `Bearer ${token}`, ...((init || {}).headers || {}) },
        signal: AbortSignal.timeout(20000),
      });
      out[name] = r.status;
    } catch (e) {
      out[name] = String(e.message || e);
    }
  };
  await probe("other_namespace_status", "/v1/rail/append?namespace=ring-not-granted", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ op: "record", payload: { doc: "x", kind: "doc-change" } }),
  });
  await probe("conversations_status", "/v1/conversations");
  await probe("mesh_status_status", "/v1/mesh/status");
  done();
} catch (e) {
  done(e);
}

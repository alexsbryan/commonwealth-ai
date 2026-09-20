// SPDX-License-Identifier: AGPL-3.0-or-later
// The phone in the room: nothing installed, no daemon, no mesh key. It reads
// the QR off the wall, opens the page the door serves, types a name, edits the
// doc and asks the room — and every one of those goes through the SAME
// `window.ring` the product ships, fetched from the door as a browser fetches
// it (`guest_door::ring_shim`), never a reimplementation of it here.
//
// What IS this instrument's own is the browser around that shim: a `location`
// carrying the link's fragment, a `localStorage` that survives a navigation
// within one origin, a `prompt` that records which element took the string,
// and a `fetch` that resolves the shim's relative paths against the page's
// origin. That is the same stand-in as ring-doc-demo.sh's in-container
// forwarder — "the browser on that machine" — and it is deliberately the only
// thing supplied, so a bug in the shim shows up here rather than being papered
// over.
//
// ONE QR serves the wall, so the scan lands on the door's INDEX and the phone
// picks an app from it, exactly as a person would. Each app is a page load at
// the same origin, which is why the name is claimed once and the second app
// never asks: the session is the door's and it is remembered per origin.
//
// Everything it "typed" is reported WITH THE ELEMENT THAT TOOK IT, so the
// census can tell the door's own prompt from a name field on an app's page.
// Reads env, writes ONE json object.
//
//   QR_SVG   the wall's QR, as bytes off the wall (the camera)
//   REPO     the repo root, for the ring-doc page's own adapter
//   NAME     the name to type, once
//   APPS     what to open, in order: `<namespace>:<kind>[:<dir>]`, comma
//            separated. `doc` is ring-doc; `expenses` is a `svrn ring new`
//            scaffold and `<dir>` is the directory it was scaffolded into,
//            which is where its own `expenses.js` is imported from.
//   EDITS    how many further edits to make on a doc (default 1)
//   ASK      a question, or empty for none
//   COLLIDE  a roster member's name to claim FIRST, expecting a refusal
//   LIAR     "1": on the first expenses app, append twice without the shim —
//            once with no name on the wire and once claiming another guest's
//            — so the stamp can be read against what the page supplied
//   PROBES   `<label>=<namespace>`, comma separated: namespaces to attempt a
//            bare append on with this bearer, recording status and sentence
//   LABEL    what to call this phone in the output
import { readFileSync } from "node:fs";

import { decodeSvgQr, gridFromSvg, quietZone } from "./qr.mjs";

const {
  QR_SVG,
  REPO,
  NAME,
  APPS = "",
  EDITS = "1",
  ASK = "",
  COLLIDE = "",
  LIAR = "",
  PROBES = "",
  LABEL = "phone",
} = process.env;
const out = { label: LABEL, typed: [], errors: [], acts: [], apps: [], claims: [], alerts: [] };
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
  const fragment = link.slice(hash + 1);
  const params = new URLSearchParams(fragment);
  const token = params.get("token");
  if (!token) throw new Error("the link's fragment names no token");
  out.expires_at = Number(params.get("exp")) || null;
  out.summary = params.get("s");
  out.token_in_query = base.includes("token");
  const origin = new URL(base).origin;
  out.origin = origin;

  // ── the browser: one origin, one storage, one person ──────────────────────
  // `localStorage` outlives a navigation, which is the whole mechanism behind
  // "the same phone is not asked its name again on the second app". A Map is
  // the honest stand-in: same origin, same jar, cleared when the phone is.
  const jar = new Map();
  globalThis.localStorage = {
    getItem: (k) => (jar.has(k) ? jar.get(k) : null),
    setItem: (k, v) => jar.set(k, String(v)),
    removeItem: (k) => jar.delete(k),
  };
  // What the person would type when something asks them. `COLLIDE` first when
  // there is one: the door refuses it and the shim asks again, which is the
  // loop a person in the room would actually walk.
  const queue = COLLIDE ? [COLLIDE, NAME] : [NAME];
  const realFetch = globalThis.fetch;
  let asked = 0;
  globalThis.window = {
    prompt: (message) => {
      // The shim re-asks whenever the door refuses a name, which is right for
      // a person and a hang for an instrument that would type the same one
      // forever. Two refusals of the name this phone came with is a finding.
      asked += 1;
      if (asked > queue.length + 2) throw new Error(`the door refused every name this phone had (${asked} asks)`);
      const typed = queue.length > 1 ? queue.shift() : queue[0];
      out.typed.push({
        // `wall` is the name the wall shows this person under; `collide` is a
        // member's name tried on purpose. Both are the door's prompt.
        leg: COLLIDE && typed === COLLIDE ? "collide" : "wall",
        string: typed,
        note: COLLIDE && typed === COLLIDE ? "a roster member's name, tried on purpose" : "the name, typed on the phone",
        // WHICH element took the string. The bar turns on this being the
        // door's own prompt and never a field on an app's page.
        element: `window.prompt, from the door's shim: ${JSON.stringify(message)}`,
      });
      return typed;
    },
    alert: (m) => out.alerts.push(String(m)),
  };
  globalThis.location = { hash: `#${fragment}` };
  globalThis.fetch = (u, o) =>
    realFetch(String(u).startsWith("http") ? String(u) : origin + String(u), {
      ...(o || {}),
      signal: AbortSignal.timeout(300000),
    });

  // ── the wall's index: one scan, every app the owner put on the wall ───────
  // A WALL grant's link is the door's index; a `--rail <ns>` grant's link is
  // that one app's page. Both are the same door, so the app path is composed
  // from the prefix either way and only the index read is conditional.
  const wallBase = base.endsWith("/ring/") ? base : base.replace(/\/ring\/[^/]+\/?$/, "/ring/");
  out.narrowed = wallBase !== base;
  if (!out.narrowed) {
    const index = await fetch(base);
    const indexHtml = await index.text();
    out.index = {
      status: index.status,
      // The door's index links each app by its namespace and completes the
      // href with the fragment in the browser; what a scanner sees is the list.
      apps: [...indexHtml.matchAll(/<a class="app" href="([^"/]+)\//g)].map((m) => m[1]),
    };
  }

  /// Open one app at this door and hand back its `window.ring`.
  ///
  /// A page load, not a re-scan: same origin, same fragment, and the shim is
  /// re-evaluated exactly as a navigation re-evaluates it. The name is asked
  /// by that shim or by nothing.
  const open = async (namespace) => {
    const pageUrl = `${wallBase}${namespace}/`;
    const page = await fetch(pageUrl);
    const html = await page.text();
    const shimSrc = `/ring/${namespace}/__ring.js`;
    const record = {
      namespace,
      page: { status: page.status, url: pageUrl },
      has_shim: html.includes(`<script src="${shimSrc}">`),
      prompts_before: out.typed.filter((t) => t.element && t.element.startsWith("window.prompt")).length,
    };
    out.apps.push(record);
    if (!page.ok || !record.has_shim) throw new Error(`the door served no ring page for ${namespace} (${page.status})`);
    const shimResp = await fetch(`${origin}${shimSrc}`);
    const shim = await shimResp.text();
    record.shim = { status: shimResp.status, bytes: shim.length };
    if (!shimResp.ok) throw new Error(`the door served no shim for ${namespace} (${shimResp.status})`);
    globalThis.window.ring = undefined;
    new Function(shim)();
    const ring = globalThis.window.ring;
    if (!ring) throw new Error(`the door's shim defined no window.ring for ${namespace}`);
    return { ring, record };
  };

  /// The claim, watched. The shim asks the door for a session the first time
  /// anything is sent; `prompts_after` minus `prompts_before` is how many
  /// times this app asked for a name, and on the second app it must be zero.
  const claimed = (record) => {
    record.prompts_after = out.typed.filter((t) => t.element && t.element.startsWith("window.prompt")).length;
    record.asked_a_name = record.prompts_after - record.prompts_before;
    record.guest = globalThis.window.ring ? globalThis.window.ring.guest : null;
    out.claims.push({ namespace: record.namespace, asked: record.asked_a_name, guest: record.guest });
  };

  // ── ring-doc, as the page holds it ────────────────────────────────────────
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

  const walkDoc = async (ring, record) => {
    const ydoc = new Y.Doc();
    const applied = new Set();
    const frag = ydoc.getXmlFragment(A.PROSE_FRAGMENT);
    const log = await ring.log();
    claimed(record);
    record.acts_before = A.decodeActs(log, fold).acts.length;
    out.namespace = log.namespace || null;
    out.roster = Object.keys((log.roster || {}).members || {});
    A.applyNew(ydoc, A.decodeActs(log, fold).acts, applied);

    let update = null;
    ydoc.on("update", (u) => {
      update = u;
    });
    // ring-doc STILL carries its own guest handling — the name it volunteers
    // into the payload, which `rg-2-ring-doc-sheds-its-guest-code` deletes.
    // The page reuses the name it already holds; nothing asks for it again,
    // which is why this is reported as SUPPLIED and never as typed.
    record.doc_guest_supplied = record.guest;
    /// One edit, recorded through the shim. Returns the act.
    ///
    /// The paragraph is created here when the doc is empty, which is what the
    /// editor does for the first person to type into a fresh ring — the wall
    /// shows a blank page until somebody writes, and in this room the first
    /// somebody is a guest. It goes in the SAME transaction as the text so one
    /// act carries both; two would put a paragraph on every screen that nobody
    /// had typed into.
    const write = async (word) => {
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
      return ring.record(A.changeAct(Y.mergeUpdates([update]), A.DOC_ID, record.guest));
    };

    // The first word is the `wall` leg: the act the wall shows this person
    // under for the first time. The edits that follow are the `edit` leg.
    let t = now();
    const first = await write("here");
    out.acts.push({ leg: "wall", ns: record.namespace, id: first.id, actor: first.actor, at: t, took_s: +(now() - t).toFixed(2) });
    for (let i = 0; i < Number(EDITS); i += 1) {
      const word = `edit${i}`;
      out.typed.push({ leg: "edit", string: word, note: "a word typed into the doc", element: "the doc's own prose editor" });
      t = now();
      const act = await write(word);
      out.acts.push({ leg: "edit", ns: record.namespace, id: act.id, actor: act.actor, at: t, took_s: +(now() - t).toFixed(2) });
    }
  };

  /// The scaffolded expenses app — `svrn ring new`, unmodified, and nothing in
  /// it has ever heard of a guest. The phone drives the SAME two things the
  /// page's submit handler drives: the app's own validator, and `ring.record`.
  const walkExpenses = async (ring, record, dir) => {
    const E = await import(`file://${dir}/expenses.js`);
    const log = await ring.log();
    claimed(record);
    const roster = (log.roster || {}).members || {};
    record.roster = Object.keys(roster);
    const member = record.roster[0] || "";
    const field = (name, value) => {
      out.typed.push({
        leg: "expense",
        string: String(value),
        note: `the scaffold's own form, asking ${name}`,
        element: `form#add input[name="${name}"] on the scaffolded page`,
      });
      return value;
    };
    const act = {
      kind: E.EXPENSE,
      payer: field("payer", record.guest),
      amount_cents: Math.round(parseFloat(field("amount", "40.00")) * 100),
      description: field("description", "propane"),
      participants: field("participants", [record.guest, member].filter(Boolean).join(", "))
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
    };
    // The scaffold's own gate, run exactly where `templates/app.js` runs it.
    record.writable = E.writable(act, roster);
    record.gaps = E.validate(act, roster).map((g) => g.message);
    record.describes = E.describe(act);
    if (!record.writable) throw new Error(`the scaffold refused its own act: ${record.gaps.join("; ")}`);

    // 1 — record.
    let t = now();
    const recorded = await ring.record(act);
    out.acts.push({ leg: "expense", ns: record.namespace, id: recorded.id, actor: recorded.actor, kind: "record", at: t, took_s: +(now() - t).toFixed(2) });

    // 2 — a correction that RESTATES it: the amount was wrong.
    const restated = { ...act, amount_cents: 4500, description: "propane" };
    t = now();
    const corrected = await ring.correct(recorded.id, restated);
    out.acts.push({ leg: "expense-correct", ns: record.namespace, id: corrected.id, actor: corrected.actor, kind: "correct-with-replacement", at: t, took_s: +(now() - t).toFixed(2) });

    // 3 — the retraction: a correction with NO replacement, which is the act
    // kind a reserved payload key could never have stamped (D1's whole
    // argument). Nothing inside a payload carries the name here; the field
    // beside it does.
    t = now();
    const retracted = await ring.correct(corrected.id, null);
    out.acts.push({ leg: "expense-retract", ns: record.namespace, id: retracted.id, actor: retracted.actor, kind: "correct-with-none", at: t, took_s: +(now() - t).toFixed(2) });
    record.act_kinds = ["record", "correct-with-replacement", "correct-with-none"];

    // ── the two liar pages ──────────────────────────────────────────────────
    // NOT through the shim: the shim sends no name and the question is what a
    // page that DID would get. Both carry this phone's session header, so the
    // door knows who is asking and the body is the only thing that differs.
    if (LIAR === "1") {
      const lie = async (label, body) => {
        const t0 = now();
        const r = await realFetch(`${origin}/v1/rail/append?namespace=${encodeURIComponent(record.namespace)}`, {
          method: "POST",
          headers: {
            authorization: `Bearer ${token}`,
            "content-type": "application/json",
            "x-ring-session": JSON.parse(jar.get("ring.session") || "{}").handle || "",
          },
          body: JSON.stringify(body),
          signal: AbortSignal.timeout(60000),
        });
        const v = await r.json().catch(() => null);
        out.liar = out.liar || {};
        out.liar[label] = { status: r.status, id: (v || {}).id || null, at: t0, sent: body.on_behalf_of ?? null };
        if (v && v.id) {
          out.acts.push({ leg: `liar-${label}`, ns: record.namespace, id: v.id, at: t0, kind: "record" });
        }
      };
      // (a) a page that sends no name at all — the scaffold's own shape.
      await lie("silent", { op: "record", payload: { ...act, description: "a page that names nobody" } });
      // (b) a page that claims somebody else's.
      await lie("claims_another", {
        op: "record",
        on_behalf_of: "Somebody Else",
        payload: { ...act, description: "a page that claims another name" },
      });
    }
  };

  // ── the apps, in order ────────────────────────────────────────────────────
  for (const spec of APPS.split(",").map((s) => s.trim()).filter(Boolean)) {
    const [namespace, kind, dir] = spec.split(":");
    const { ring, record } = await open(namespace);
    record.kind = kind;
    if (kind === "expenses") await walkExpenses(ring, record, dir);
    else await walkDoc(ring, record);
    // The ask rides the app the question was asked from; one is enough.
    if (ASK && !out.ask) {
      out.typed.push({ leg: "ask", string: ASK, note: "the question, typed on the phone", element: "the page's ask box" });
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
    await sleep(50);
  }
  // The collision, as the door answered it: the shim loops on 409 and the
  // door's own sentence is what it alerted with.
  if (COLLIDE) {
    out.collision = {
      refused: out.alerts.length > 0,
      error: out.alerts[0] || null,
      // The name it ended up with, which must never be the one it tried.
      holds: out.claims.length ? out.claims[0].guest : null,
    };
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
      const text = await r.text().catch(() => "");
      out[name] = r.status;
      return { status: r.status, body: text.slice(0, 300) };
    } catch (e) {
      out[name] = String(e.message || e);
      return { status: out[name], body: "" };
    }
  };
  await probe("other_namespace_status", "/v1/rail/append?namespace=ring-not-granted", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ op: "record", payload: { doc: "x", kind: "doc-change" } }),
  });
  await probe("conversations_status", "/v1/conversations");
  await probe("mesh_status_status", "/v1/mesh/status");

  // The wall's declaration, asked three ways: a namespace nobody declared, one
  // this daemon owns, and one declared read-only. Each must come back refused
  // WITH A SENTENCE naming the namespace — a bearer that reaches the whole
  // rail would answer all three.
  for (const spec of PROBES.split(",").map((s) => s.trim()).filter(Boolean)) {
    const [label, namespace] = spec.split("=");
    out.probes = out.probes || {};
    const r = await probe(`probe_${label}`, `/v1/rail/append?namespace=${encodeURIComponent(namespace)}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ op: "record", payload: { kind: "expense", note: "a stranger's append" } }),
    });
    out.probes[label] = { namespace, status: r.status, says: r.body, names_it: r.body.includes(namespace) };
  }
  done();
} catch (e) {
  done(e);
}

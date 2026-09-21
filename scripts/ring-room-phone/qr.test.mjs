// SPDX-License-Identifier: AGPL-3.0-or-later
// The camera's own tests. `node --test scripts/ring-room-phone`.
//
// The fixture is a real QR code of a real guest link, written in fast_qr's
// exact SVG spelling (`M<x>,<y>h1v1h-1` commands, square viewBox, 4-module
// quiet zone) — generated 2026-09-19 with the `qrcode` npm encoder, which is
// NOT this file's decoder, so a round trip here is two independent
// implementations agreeing rather than one talking to itself. Whether
// `svrn mesh grant --qr-svg` still spells it this way is asserted by the room
// demo, which decodes the wall's actual SVG.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { decodeSvgQr, gridFromSvg, quietZone } from "./qr.mjs";

const FIXTURE = new URL("./wall-qr.fixture.svg", import.meta.url);
const LINK =
  "http://10.89.60.11:19947/ring/#token=9f3c1b7e4a2d5f608c1e9b3a7d4f2c60" +
  "&exp=1790000000&s=models%3A+2+%C2%B7+rail%3A+ring-doc";

const svg = () => readFileSync(FIXTURE, "utf8");

test("the wall's QR decodes back to the exact guest link", () => {
  assert.equal(decodeSvgQr(svg()), LINK);
});

test("the token rides the fragment, so no server ever logs it", () => {
  const link = decodeSvgQr(svg());
  const [before, fragment] = link.split("#");
  assert.ok(fragment, "the link carries no fragment");
  assert.ok(!before.includes("token"), `the token is outside the fragment: ${before}`);
  assert.match(fragment, /(^|&)token=/);
});

test("the grid is read in module units, not pixels", () => {
  const grid = gridFromSvg(svg());
  assert.equal(grid.size, 53, "45 modules plus two 4-module quiet zones");
  assert.ok(grid.modules > 400 && grid.modules < grid.size * grid.size);
});

test("the quiet zone is four modules wide — a camera needs it", () => {
  assert.equal(quietZone(gridFromSvg(svg())), 4);
});

// MEASURED, not assumed: jsQR decodes a grid whose quiet zone was cropped
// away, because the image border stands in for it. A phone held at an angle
// against a wall does not get that border — which is why the quiet zone is
// checked HERE, by us, rather than left to the decoder to complain about.
test("a cropped render is reported as having no quiet zone", () => {
  const grid = gridFromSvg(svg());
  const q = quietZone(grid);
  const inner = grid.size - 2 * q;
  let d = "";
  for (let y = 0; y < inner; y += 1) {
    for (let x = 0; x < inner; x += 1) {
      if (grid.dark[(y + q) * grid.size + (x + q)]) d += `M${x},${y}h1v1h-1`;
    }
  }
  const cropped = `<svg viewBox="0 0 ${inner} ${inner}"><path d="${d}" fill="#000"/></svg>`;
  assert.equal(quietZone(gridFromSvg(cropped)), 0);
  assert.equal(decodeSvgQr(cropped), LINK, "the decoder tolerates it; the wall check is ours");
});

test("an svg with no modules is an error, never an empty link", () => {
  assert.throws(() => gridFromSvg('<svg viewBox="0 0 53 53"></svg>'), /no `M/);
});

test("a module outside the canvas is an error, never a silent clip", () => {
  assert.throws(
    () => gridFromSvg('<svg viewBox="0 0 8 8"><path d="M9,1h1v1h-1"/></svg>'),
    /outside the 8-module canvas/,
  );
});

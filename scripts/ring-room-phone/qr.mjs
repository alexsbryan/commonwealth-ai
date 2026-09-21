// SPDX-License-Identifier: AGPL-3.0-or-later
// The phone's camera, in software: read the wall's QR SVG and give back the
// link it carries.
//
// `svrn mesh grant --qr-svg` writes fast_qr's SVG (mesh_guest.rs `wall_qr_svg`,
// SvgBuilder with a 4-module quiet zone). fast_qr emits ONE `<path>` whose `d`
// is a run of `M<x>,<y>h1v1h-1` — one command per dark module, in MODULE units,
// already offset by the margin — inside a square `viewBox="0 0 N N"`
// (convert/svg.rs `path` + `to_str`, convert/mod.rs `Shape::square`). So the
// module grid is recoverable exactly, with no rasteriser and no rounding: the
// geometry IS the grid.
//
// What is NOT reimplemented here is the decode. The grid is painted into a
// bitmap and handed to jsQR, the same scan a camera's frame gets — so a QR
// this refuses is one a phone would refuse too.
import jsQR from "jsqr";

/// The dark-module grid of a fast_qr SVG: `{ size, dark }`, `dark[y*size+x]`.
///
/// Throws rather than returning a partial grid: a QR missing modules decodes
/// to something wrong far more often than it decodes to nothing.
export function gridFromSvg(svg) {
  const box = /viewBox="0 0 (\d+) (\d+)"/.exec(svg);
  if (!box) throw new Error("not a fast_qr svg: no integer viewBox");
  if (box[1] !== box[2]) throw new Error(`viewBox is not square: ${box[1]}x${box[2]}`);
  const size = Number(box[1]);
  const dark = new Uint8Array(size * size);
  let modules = 0;
  for (const m of svg.matchAll(/M(\d+),(\d+)h1v1h-1/g)) {
    const x = Number(m[1]);
    const y = Number(m[2]);
    if (x >= size || y >= size) throw new Error(`module ${x},${y} is outside the ${size}-module canvas`);
    dark[y * size + x] = 1;
    modules += 1;
  }
  if (modules === 0) throw new Error("no `M<x>,<y>h1v1h-1` modules in the svg");
  return { size, dark, modules };
}

/// The quiet zone the grid actually carries: the width of the all-light border.
///
/// Read rather than assumed, because it is the one property of the render that
/// a decoder needs and that a careless margin change would silently remove —
/// the phone would then fail to scan a wall that looks fine to a person.
export function quietZone({ size, dark }) {
  const light = (x, y) => dark[y * size + x] === 0;
  for (let r = 0; r < size / 2; r += 1) {
    for (let i = r; i < size - r; i += 1) {
      if (!light(i, r) || !light(i, size - 1 - r) || !light(r, i) || !light(size - 1 - r, i)) {
        return r;
      }
    }
  }
  return Math.floor(size / 2);
}

/// Decode the link out of a fast_qr SVG, as a camera would.
export function decodeSvgQr(svg, scale = 4) {
  const grid = gridFromSvg(svg);
  const { size, dark } = grid;
  const w = size * scale;
  const data = new Uint8ClampedArray(w * w * 4);
  for (let py = 0; py < w; py += 1) {
    const gy = Math.floor(py / scale) * size;
    for (let px = 0; px < w; px += 1) {
      const v = dark[gy + Math.floor(px / scale)] ? 0 : 255;
      const i = (py * w + px) * 4;
      data[i] = v;
      data[i + 1] = v;
      data[i + 2] = v;
      data[i + 3] = 255;
    }
  }
  const read = jsQR(data, w, w);
  if (!read) throw new Error(`the svg's ${size}-module grid did not decode as a QR code`);
  return read.data;
}

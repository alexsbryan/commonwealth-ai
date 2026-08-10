// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Byte offsets (what the Rust runtime speaks) → UTF-16 code-unit indices
// (what a JS string slice speaks).
//
// Two surfaces need this and they must not disagree: the reading view's
// atom overlay (`reading/ChunkRenderer.svelte`, `atom_spans`) and the
// answer-provenance strip (`answerProvenance.ts`, `answer_segments`).
// Both receive byte ranges into UTF-8 text produced by
// `sovereign-core`; a second, subtly different converter would put one
// of the two surfaces' highlights on the wrong characters for exactly
// the inputs — accented names, emoji — where a mis-highlight is most
// visible. One implementation, one name (ARCH §10.6).

/** Byte index → UTF-16 code-unit index, for one string.
 *
 *  Length is `utf8Length + 1`: the extra entry is the end-of-text
 *  sentinel, so an exclusive `end` offset one past the last byte is a
 *  valid lookup rather than an out-of-bounds read. */
export function buildByteToUtf16Map(content: string): number[] {
  const table: number[] = [];
  const encoder = new TextEncoder();
  let codeUnitIdx = 0;
  for (const ch of content) {
    // Each iteration covers one Unicode scalar value: 1 UTF-16 code
    // unit for BMP, 2 for a surrogate pair.
    const utf16Len = ch.length;
    const utf8Len = encoder.encode(ch).length;
    for (let i = 0; i < utf8Len; i++) {
      table.push(codeUnitIdx);
    }
    codeUnitIdx += utf16Len;
  }
  table.push(codeUnitIdx);
  return table;
}

/** A byte-index → UTF-16-index function for `content`.
 *
 *  ASCII strings take the identity fast path (no table allocated), which
 *  is the overwhelming majority of answers. Out-of-range indices are
 *  CLAMPED, not thrown: a backend off-by-one should mis-position a
 *  highlight by one character, never blank the message it decorates. */
export function byteIndexer(content: string): (byteIdx: number) => number {
  const needsMap = content.length !== new TextEncoder().encode(content).length;
  if (!needsMap) {
    const maxByte = content.length;
    return (byteIdx) => Math.min(Math.max(byteIdx, 0), maxByte);
  }
  const table = buildByteToUtf16Map(content);
  return (byteIdx) => table[Math.min(Math.max(byteIdx, 0), table.length - 1)];
}

/** `content` sliced by a UTF-8 byte range. */
export function byteSlice(content: string, start: number, end: number): string {
  const u16 = byteIndexer(content);
  return content.slice(u16(start), u16(end));
}

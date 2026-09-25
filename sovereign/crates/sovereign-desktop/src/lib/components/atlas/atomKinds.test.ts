// SPDX-License-Identifier: AGPL-3.0-or-later
// declaredTypeColor — the stage-0b palette for a DECLARED type's own noun.
// Deterministic (same noun → same colour across sessions and corpora, so a
// recording and a live map agree), distinguishable from the closed kind
// palette (constrained HSL family), and total (any noun hashes to a colour
// — the set is open by design, ARCH principle 9).
import { describe, it, expect } from "vitest";
import { declaredTypeColor, atomTypeColor } from "./atomKinds";

describe("declaredTypeColor", () => {
  it("is deterministic for the same noun", () => {
    for (const noun of ["hoard", "mint", "data_type", "obligation"]) {
      expect(declaredTypeColor(noun)).toBe(declaredTypeColor(noun));
    }
  });

  it("separates the declared nouns a stage corpus actually carries", () => {
    // The ANS declaration's four: a map shot colours by these, and any
    // collision would make the shot lie.
    const nouns = ["hoard", "mint", "ruler", "coin"];
    const colors = nouns.map(declaredTypeColor);
    expect(new Set(colors).size).toBe(nouns.length);
  });

  it("renders in the constrained HSL family, not the kind palette's space", () => {
    const c = declaredTypeColor("hoard");
    expect(c).toMatch(/^hsl\(\d+, 62%, 46%\)$/);
    // And it is not accidentally one of the closed palette's hexes.
    expect(c).not.toBe(atomTypeColor("Entity"));
  });

  it("differs for nouns that differ by one character", () => {
    expect(declaredTypeColor("mint")).not.toBe(declaredTypeColor("mints"));
  });
});

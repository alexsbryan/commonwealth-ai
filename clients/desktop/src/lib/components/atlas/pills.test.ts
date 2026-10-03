// SPDX-License-Identifier: AGPL-3.0-or-later
//
// derivePills — the author's nouns, and the roll-up.
//
// Two facts drive every case here and neither is guessable from the
// wire alone:
//
//  1. `subtype_counts` are OWN counts. On the real `wessex-hoard`
//     atlas, `coin` is 13 and `sceatta` is 2, and `sceatta specializes
//     coin` — so "how many coins" is 15 and nothing in the census says
//     so. The hierarchy rides in `declared_types[].specializes`.
//  2. A `role_of` type is declared `kind = "entity"` and lands as
//     STATE atoms (`ruler role_of person`: the atoms are people, the
//     roles are states). So a pill for `ruler` must filter by subtype,
//     never by kind, or it finds nothing.
//
// The numbers below are the live ones from
// `~/.svrnmesh/indexes/wessex-hoard/atlas/_summary.json`.
import { describe, it, expect } from "vitest";
import { derivePills } from "./pills";
import type { DeclaredTypeRow } from "../../types";

// Alphabetical, because that is the order the wire actually delivers:
// `_summary.json` v4 carries the declaration as a BTreeMap, so the
// recipe's own sequence (coin, sceatta, ruler, mint, attribution) is
// gone by the time a viewer sees it. Writing the fixture in declaration
// order would be testing a shape the backend cannot produce.
const WESSEX_TYPES: DeclaredTypeRow[] = [
  { name: "attribution", kind: "claim" },
  { name: "coin", kind: "entity", identity_criterion: "external:catalogue_ref" },
  { name: "mint", kind: "entity" },
  { name: "ruler", kind: "entity" },
  { name: "sceatta", kind: "entity", specializes: "coin" },
];

const WESSEX_SUBTYPE_COUNTS = {
  attribution: 37,
  coin: 13,
  mint: 3,
  person: 13,
  place: 2,
  ruler: 9,
  sceatta: 2,
  work: 4,
};

const WESSEX_ATOM_COUNTS = {
  Entity: 37,
  Event: 4,
  State: 27,
  Relation: 19,
  Claim: 37,
  Question: 22,
} as const;

function wessex() {
  return derivePills({
    declaredTypes: WESSEX_TYPES,
    subtypeCounts: WESSEX_SUBTYPE_COUNTS,
    atomCounts: WESSEX_ATOM_COUNTS,
    totalAtoms: 146,
  });
}

const byKey = (key: string) => (p: { key: string }) => p.key === key;

describe("derivePills — declared corpus", () => {
  it("rolls a parent's count up over its specializations", () => {
    const coin = wessex().find(byKey("subtype:coin"));
    expect(coin?.count).toBe(15); // 13 coin + 2 sceatta
    expect(coin?.ownCount).toBe(13);
  });

  /// The badge and the click have to be the same question. The backend
  /// filter is exact and never walks `specializes`, so the pill names
  /// every descendant — otherwise a badge reading 15 opens a list of 13.
  it("names the whole family in the filter, so the badge matches the list", () => {
    const coin = wessex().find(byKey("subtype:coin"));
    expect(coin?.filter).toEqual({
      scope: "subtype",
      subtypes: ["coin", "sceatta"],
    });
    // The count the badge shows is the sum over exactly those names.
    const named =
      coin?.filter.scope === "subtype" ? coin.filter.subtypes : [];
    const census: Record<string, number> = WESSEX_SUBTYPE_COUNTS;
    const summed = named.reduce((n, k) => n + (census[k] ?? 0), 0);
    expect(summed).toBe(coin?.count);
  });

  it("leaves a childless type's count alone and omits ownCount", () => {
    const mint = wessex().find(byKey("subtype:mint"));
    expect(mint?.count).toBe(3);
    expect(mint?.ownCount).toBeUndefined();

    const sceatta = wessex().find(byKey("subtype:sceatta"));
    expect(sceatta?.count).toBe(2);
    expect(sceatta?.ownCount).toBeUndefined();
  });

  it("counts a role_of type from the subtype census, not its declared kind", () => {
    // `ruler` is declared kind=entity and lands as 9 STATE atoms. A
    // reader that looked inside the Entity bucket would report zero
    // for a role that landed perfectly.
    const ruler = wessex().find(byKey("subtype:ruler"));
    expect(ruler?.count).toBe(9);
    expect(ruler?.filter).toEqual({ scope: "subtype", subtypes: ["ruler"] });
  });

  /// The wire hands these over alphabetically — `_summary.json` stores
  /// `ontology.declared` as a sorted map — which puts `mint` and `ruler`
  /// between `coin` and the `sceatta` that specializes it. Grouping is
  /// derivable from `specializes`, so the viewer does it.
  ///
  /// Falsifier: iterate `declaredTypes` directly and `sceatta` lands last.
  it("keeps a family together, parent first, whatever order the wire used", () => {
    const declared = wessex()
      .filter((p) => p.declared)
      .map((p) => p.label);
    expect(declared).toEqual([
      "attribution",
      "coin",
      "sceatta",
      "mint",
      "ruler",
    ]);
  });

  it("filters declared pills by subtype ONLY, never paired with a kind", () => {
    for (const p of wessex().filter((p) => p.declared)) {
      expect(p.filter.scope).toBe("subtype");
    }
  });

  it("puts the author's nouns before the system's kinds", () => {
    const keys = wessex().map((p) => p.key);
    expect(keys.slice(0, 6)).toEqual([
      "all",
      "subtype:attribution",
      "subtype:coin",
      // `sceatta` follows the `coin` it specializes, though the wire
      // hands them over alphabetically with `mint` and `ruler` between.
      "subtype:sceatta",
      "subtype:mint",
      "subtype:ruler",
    ]);
    expect(keys[6]).toBe("kind:Entity");
  });

  it("keeps the generic kind pills so undeclared atoms stay reachable", () => {
    // 19 of wessex-hoard's 37 Entities are `person`/`place`/`work` —
    // real atoms the author never declared. Dropping "the kinds a
    // declaration covers" would leave them reachable only by name
    // search.
    const entity = wessex().find(byKey("kind:Entity"));
    expect(entity?.count).toBe(37);
    expect(entity?.filter).toEqual({ scope: "kind", kind: "Entity" });
    expect(entity?.declared).toBe(false);
  });

  it("counts a declared-but-never-extracted type as zero, not unknown", () => {
    // The headline failure of the whole ontology program: the type is
    // present in the recipe and absent from the atoms. A missing badge
    // would read as "not measured" (§18.3).
    const pills = derivePills({
      declaredTypes: [{ name: "hoard", kind: "entity" }],
      subtypeCounts: {},
      atomCounts: {},
      totalAtoms: 0,
    });
    expect(pills.find(byKey("subtype:hoard"))?.count).toBe(0);
  });

  it("survives a cyclic specializes declaration without hanging", () => {
    const pills = derivePills({
      declaredTypes: [
        { name: "a", kind: "entity", specializes: "b" },
        { name: "b", kind: "entity", specializes: "a" },
      ],
      subtypeCounts: { a: 2, b: 3 },
    });
    expect(pills.find(byKey("subtype:a"))?.count).toBe(5);
    expect(pills.find(byKey("subtype:b"))?.count).toBe(5);
  });

  it("rolls up through a grandchild", () => {
    const pills = derivePills({
      declaredTypes: [
        { name: "coin", kind: "entity" },
        { name: "sceatta", kind: "entity", specializes: "coin" },
        { name: "porcupine", kind: "entity", specializes: "sceatta" },
      ],
      subtypeCounts: { coin: 10, sceatta: 2, porcupine: 1 },
    });
    expect(pills.find(byKey("subtype:coin"))?.count).toBe(13);
    expect(pills.find(byKey("subtype:sceatta"))?.count).toBe(3);
  });
});

describe("derivePills — undeclared corpus (back-compat)", () => {
  // SEP, Wikipedia and Enron declare nothing. Their filter row must be
  // byte-for-byte what it was before ontology v1 existed.
  const TODAYS_ROW = [
    "All",
    "Entity",
    "Event",
    "State",
    "Relation",
    "Claim",
    "Question",
    "Config",
    "Argument",
  ];

  it("renders exactly today's eight kinds plus All", () => {
    const pills = derivePills({ totalAtoms: 51280 });
    expect(pills.map((p) => p.label)).toEqual(TODAYS_ROW);
    expect(pills.every((p) => !p.declared)).toBe(true);
  });

  it("does the same for an empty declared_types array", () => {
    const pills = derivePills({
      declaredTypes: [],
      subtypeCounts: {},
      atomCounts: { Entity: 5 },
      totalAtoms: 5,
    });
    expect(pills.map((p) => p.label)).toEqual(TODAYS_ROW);
  });

  it("renders no badge at all when there is no census", () => {
    // Absent is reported as absent — a `0` the corpus never claimed
    // would also disable the tab.
    const pills = derivePills({});
    expect(pills.every((p) => p.count === undefined)).toBe(true);
  });

  it("adds Position / Opposition / Asset only when the corpus has some", () => {
    expect(
      derivePills({ atomCounts: { Entity: 3 } }).map((p) => p.key),
    ).not.toContain("kind:Position");

    const withPositions = derivePills({
      atomCounts: { Entity: 3, Position: 4, Asset: 0 },
    });
    expect(withPositions.find(byKey("kind:Position"))?.count).toBe(4);
    // Asset counted zero stays out — an always-empty tab is noise.
    expect(withPositions.map((p) => p.key)).not.toContain("kind:Asset");
  });
});

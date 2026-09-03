// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The browse filter row, derived — "the user sees their own nouns".
//
// A corpus that declared `coin`, `sceatta specializes coin`, `mint`,
// `ruler role_of person` and `attribution` should offer THOSE as
// filters, not "Entity / Claim / State". A corpus that declared
// nothing must look exactly as it did before this file existed.
//
// Pure and total: no fetches, no stores, no Svelte. The pill row is
// the visible consequence of three wire facts (declared types, the
// subtype census, the kind census) and getting it wrong is a silent
// mislabelling, so it is decided in one testable function rather than
// in template expressions (ARCH §10.6).
import type { AtomType, DeclaredTypeRow } from "../../types";
import {
  ATOM_TYPE_LABEL,
  ATOM_TYPE_ORDER,
  ATOM_TYPE_ORDER_SPARSE,
} from "./atomKinds";

/** What clicking a pill narrows the list to. Maps 1:1 onto the
 *  independent fields of the backend's `AtomFilter`. */
export type PillFilter =
  | { scope: "all" }
  | { scope: "kind"; kind: AtomType }
  | { scope: "subtype"; subtypes: string[] };

export interface AtomPill {
  /** Stable identity for `{#each}` and for comparing against the
   *  active selection. `"all"`, `"kind:Entity"`, `"subtype:coin"`. */
  key: string;
  label: string;
  filter: PillFilter;
  /** Badge number. `undefined` means "not known here" — the census
   *  was absent from the wire — which renders no badge at all rather
   *  than a zero the corpus never claimed (§18.3). */
  count?: number;
  /** For a declared type with descendants, the count of atoms whose
   *  subtype is EXACTLY this name. Shown only as an explanatory aside:
   *  `count` is what clicking returns, because the filter names the
   *  whole family. `undefined` when it equals `count`. */
  ownCount?: number;
  /** True for the author's own nouns, false for the generic kinds.
   *  Drives the visual split between the two halves of the row. */
  declared: boolean;
}

export interface PillInputs {
  /** From `AtlasCorpusSummary.declared_types`. Empty/absent → the
   *  generic kind pills alone, exactly as before ontology v1. */
  declaredTypes?: DeclaredTypeRow[];
  /** From `AtlasCorpusSummary.subtype_counts`. OWN counts — the
   *  roll-up over `specializes` happens here. */
  subtypeCounts?: Record<string, number>;
  /** From `AtlasCorpusSummary.atom_counts`. */
  atomCounts?: Partial<Record<AtomType, number>>;
  /** From `AtlasCorpusSummary.total_atoms`. */
  totalAtoms?: number;
}

/** Every declared type that specializes `name`, transitively.
 *
 *  Guarded against a cycle (`a specializes b specializes a`) by a
 *  seen-set: the declaration is authored by hand and validation lives
 *  in the recipe parser, so a viewer that hangs the UI on bad input
 *  would be blaming the wrong layer. */
function descendantsOf(
  name: string,
  childrenOf: Map<string, string[]>,
): string[] {
  const out: string[] = [];
  const seen = new Set<string>([name]);
  const queue = [...(childrenOf.get(name) ?? [])];
  while (queue.length > 0) {
    const next = queue.shift() as string;
    if (seen.has(next)) continue;
    seen.add(next);
    out.push(next);
    queue.push(...(childrenOf.get(next) ?? []));
  }
  return out;
}

/** The filter row for one corpus.
 *
 *  Order: `All`, then the declared types in the order the summary gave
 *  them, then the generic kinds.
 *
 *  That order is ALPHABETICAL today, not declaration order: the v4
 *  `_summary.json` carries the declaration as a `BTreeMap`, so the
 *  recipe's sequence is gone before this function is reachable. The
 *  visible cost is that a parent and its specializations do not sit
 *  together — `coin` … `sceatta` with `mint` and `ruler` between them.
 *  Fixing it is a Rust-side change to the sidecar; this function
 *  preserves whatever order it is handed rather than re-sorting, so it
 *  needs no change when that lands.
 *
 *  The generic kinds are kept even for a declared corpus — dropping
 *  "the kinds a declaration covers" would strand every atom the author
 *  did not classify (on `wessex-hoard` that is 19 of 37 Entities:
 *  `person`, `place`, `work`), leaving them reachable only by name
 *  search. */

export function derivePills(inputs: PillInputs): AtomPill[] {
  const {
    declaredTypes = [],
    subtypeCounts,
    atomCounts,
    totalAtoms,
  } = inputs;

  const pills: AtomPill[] = [
    { key: "all", label: "All", filter: { scope: "all" }, count: totalAtoms, declared: false },
  ];

  const childrenOf = new Map<string, string[]>();
  for (const t of declaredTypes) {
    if (!t.specializes) continue;
    const kids = childrenOf.get(t.specializes) ?? [];
    kids.push(t.name);
    childrenOf.set(t.specializes, kids);
  }

  // Parents before their own specializations, each family kept together.
  // The order the wire hands us is ALPHABETICAL — `_summary.json` carries
  // `ontology.declared` as a sorted map, so the recipe's own sequence is
  // gone before the viewer sees it — which separates `sceatta` from the
  // `coin` it specializes by putting `mint` and `ruler` between them.
  // Grouping is derivable from `specializes`, which is already here, so it
  // needs no change to the sidecar. Types whose parent is not declared in
  // this corpus are roots, so nothing is dropped.
  const declaredNames = new Set(declaredTypes.map((t) => t.name));
  const ordered: DeclaredTypeRow[] = [];
  const emitted = new Set<string>();
  const emit = (t: DeclaredTypeRow) => {
    if (emitted.has(t.name)) return;
    emitted.add(t.name);
    ordered.push(t);
    for (const kid of childrenOf.get(t.name) ?? []) {
      const row = declaredTypes.find((d) => d.name === kid);
      if (row) emit(row);
    }
  };
  for (const t of declaredTypes) {
    if (!t.specializes || !declaredNames.has(t.specializes)) emit(t);
  }
  // A cycle (`a specializes b specializes a`) leaves every member with a
  // declared parent, so none is a root and the walk above emits none of
  // them. Dropping a declared type from the row would be worse than
  // showing it in an odd place, so anything unreached is appended in the
  // order it arrived. `validate` should refuse a cyclic declaration long
  // before here; this is the viewer refusing to lose data if it does not.
  for (const t of declaredTypes) emit(t);

  for (const t of ordered) {
    // A declared type with no atoms is counted 0, not left blank: "you
    // declared this and the build produced none of it" is the headline
    // failure the whole ontology program exists to surface, and a
    // missing badge would read as "unknown".
    const own = subtypeCounts?.[t.name] ?? 0;
    const kids = descendantsOf(t.name, childrenOf);
    const rolled = kids.reduce((sum, k) => sum + (subtypeCounts?.[k] ?? 0), own);
    pills.push({
      key: `subtype:${t.name}`,
      label: t.name,
      // The family, named. The server never walks `specializes`, so the
      // filter has to carry every descendant explicitly — and because it
      // does, the badge below and the rows a click returns are the same
      // number. Naming only `t.name` here is what made them disagree.
      filter: { scope: "subtype", subtypes: [t.name, ...kids] },
      count: rolled,
      // Kept as an aside, not a correction: `count` is what clicking
      // returns. This is "and 13 of the 15 are coins themselves".
      ownCount: rolled === own ? undefined : own,
      declared: true,
    });
  }

  for (const t of ATOM_TYPE_ORDER) {
    pills.push({
      key: `kind:${t}`,
      label: ATOM_TYPE_LABEL[t],
      filter: { scope: "kind", kind: t },
      count: atomCounts?.[t],
      declared: false,
    });
  }
  for (const t of ATOM_TYPE_ORDER_SPARSE) {
    const count = atomCounts?.[t];
    if (!count) continue;
    pills.push({
      key: `kind:${t}`,
      label: ATOM_TYPE_LABEL[t],
      filter: { scope: "kind", kind: t },
      count,
      declared: false,
    });
  }

  return pills;
}

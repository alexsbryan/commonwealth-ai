// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The atom KINDS, as data. One home for the closed set the atlas UI
// iterates, labels and colours.
//
// This existed FIVE times before 2026-09-02 — `ATOM_TYPE_LABEL` in
// `AtlasIndex`, `AtlasCorpusView`, `AtomDetail` and `AtomLink`, plus
// `TYPE_COLOR` in `AtlasGraph` — and the copies had already drifted:
// four of them knew eight kinds and the graph's knew eleven, so a
// Position atom drew a green node in the map and rendered a BLANK pill
// with a BLANK body in the inspector. Widening the union to eleven
// meant touching every copy, which is the moment to have one (ARCH
// §10.6: one decider, one name).
//
// Mirrors `corpus_engine::enrichment::atlas::atoms::AtomType`, whose
// own `AtomType::ALL` is the Rust side of this same rule.
import type { AtomType } from "../../types";

/** Compact label per kind. Deliberately NOT the serde spelling:
 *  `ArgumentReconstruction` is "Argument" and `Configuration` is
 *  "Config" because these render in chips, tabs and pills where the
 *  full name does not fit. The atom-detail header used to spell
 *  `Configuration` in full from its own private copy; it now shares
 *  this one. */
export const ATOM_TYPE_LABEL: Record<AtomType, string> = {
  Entity: "Entity",
  Event: "Event",
  State: "State",
  Relation: "Relation",
  Claim: "Claim",
  Question: "Question",
  Configuration: "Config",
  ArgumentReconstruction: "Argument",
  Position: "Position",
  Opposition: "Opposition",
  Asset: "Asset",
};

export function atomTypeLabel(t: AtomType): string {
  // A wire value outside the union (an older/newer backend) falls back
  // to the raw tag rather than rendering an empty chip — the exact
  // failure the three missing kinds produced.
  return ATOM_TYPE_LABEL[t] ?? String(t);
}

/** The eight kinds the browse pills have always offered, in their
 *  established order.
 *
 *  Position / Opposition / Asset are NOT here: they are sparse,
 *  argumentative-extension and asset-substrate kinds that most corpora
 *  never emit, and adding three permanently-empty tabs to every corpus
 *  would be a regression for the many to serve the few. They join the
 *  pill row only when a corpus actually has some — see
 *  [`ATOM_TYPE_ORDER_SPARSE`] and `derivePills`. */
export const ATOM_TYPE_ORDER: readonly AtomType[] = [
  "Entity",
  "Event",
  "State",
  "Relation",
  "Claim",
  "Question",
  "Configuration",
  "ArgumentReconstruction",
] as const;

/** The three kinds shown only when counted non-zero. */
export const ATOM_TYPE_ORDER_SPARSE: readonly AtomType[] = [
  "Position",
  "Opposition",
  "Asset",
] as const;

/** All eleven, base kinds first. For a consumer that already filters
 *  by count and so has no reason to hide the sparse three — the atlas
 *  index's per-corpus count strip, which showed nothing at all for a
 *  Position-carrying corpus while its eight-kind list was private. */
export const ATOM_TYPE_ORDER_ALL: readonly AtomType[] = [
  ...ATOM_TYPE_ORDER,
  ...ATOM_TYPE_ORDER_SPARSE,
];

/** Node colour per kind for the Atlas Map. Keyed loosely (`string`)
 *  because the graph's `AtlasNode.atom_type` is the raw serde tag off
 *  the wire, not the narrowed union. */
export const ATOM_TYPE_COLOR: Record<string, string> = {
  Entity: "#7c9cff",
  Claim: "#caa45a",
  Question: "#5ec8c8",
  ArgumentReconstruction: "#b98cff",
  Position: "#7ed492",
  Opposition: "#e0764a",
  Event: "#d98ab0",
  State: "#9aa0b5",
  Relation: "#8a7fbd",
  Configuration: "#c0c8d8",
  Asset: "#6b7280",
};

export function atomTypeColor(t: string): string {
  return ATOM_TYPE_COLOR[t] ?? "#9aa0b5";
}

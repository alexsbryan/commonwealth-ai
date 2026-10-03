// SPDX-License-Identifier: AGPL-3.0-or-later
// Presentation helpers for a notebook's `source_kind` — shared by the
// Library shelf cards and the notebook-detail header so the label/title
// stay in one place. The backend may emit kinds beyond the narrowed
// `NotebookSourceKind`; unknown values fall back to the generic
// "installed" presentation.

import type { NotebookSourceKind } from "../../types";

const KNOWN: NotebookSourceKind[] = [
  "folder",
  "obsidian",
  "watched",
  "catalog",
  "installed",
];

/** Narrow an arbitrary backend string to a known kind, defaulting to
 *  `"installed"` (the catch-all for recipe / CLI / mesh-app / import). */
export function normalizeKind(kind: string): NotebookSourceKind {
  return (KNOWN as string[]).includes(kind)
    ? (kind as NotebookSourceKind)
    : "installed";
}

/** Short chip label for the source kind. */
export function kindLabel(kind: string): string {
  switch (normalizeKind(kind)) {
    case "folder":
      return "Folder";
    case "obsidian":
      return "Obsidian";
    case "watched":
      return "Watched";
    case "catalog":
      return "Catalog";
    case "installed":
      return "Installed";
  }
}

/** Longer hover/title text explaining where the notebook came from. */
export function kindTitle(kind: string): string {
  switch (normalizeKind(kind)) {
    case "folder":
      return "Built from a folder of documents you added";
    case "obsidian":
      return "Built from an Obsidian vault";
    case "watched":
      return "A folder kept in sync — edits flow into the index";
    case "catalog":
      return "Installed from the public catalog";
    case "installed":
      return "Installed from a recipe, import, or the mesh";
  }
}

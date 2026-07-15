// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Regression test for the Explore "Loading atom…" hang (2026-07-14).
//
// An atom whose `related` list references the SAME neighbour via two
// edges (e.g. two edge_types) produces two rows with an identical
// `atom_id`. The list used to be keyed `{#each detail.related as r
// (r.atom_id)}`, so Svelte 5 threw a FATAL `each_key_duplicate` that
// aborted the ENTIRE component render. The data loaded fast (~500ms) but
// the view never painted — the user saw an infinite "Loading atom…"
// spinner. Backend timing looked perfect; only the webview console
// showed the crash.
//
// This renders that exact shape and asserts BOTH related rows appear —
// i.e. the render did not crash. It fails on the old atom_id-keyed code
// and passes on the index-keyed fix.
import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import AtomDetail from "./AtomDetail.svelte";
import type { AtomDetail as AtomDetailData } from "../../types";

vi.mock("../../api", () => ({
  atlasGetAtomDetail: vi.fn(),
}));
const api = await import("../../api");

function detailWithDuplicateNeighbour(): AtomDetailData {
  const relatedRow = (edge_type: string) => ({
    // SAME neighbour id on both rows — an atom can relate to one
    // neighbour through several edge types, so atom_id is NOT unique.
    atom_id: "entity-dup",
    atom_type: "Entity" as const,
    display_name: "Repeated Neighbour",
    edge_type,
    role: "target",
    confidence: 0.9,
  });
  return {
    corpus_id: "wikipedia",
    atom_id: "entity-0255",
    stable_key: "sk-1",
    atom_type: "Entity",
    display_name: "2026 Negombo prison riot",
    salience: 0.5,
    atom: {
      atom_type: "Entity",
      data: {
        id: "entity-0255",
        canonical_name: "2026 Negombo prison riot",
        aliases: [],
        entity_type: "event",
        first_appearance: { chunk_id: "c1" },
        description: "A prison riot.",
        salience: 0.5,
        enrichment_depth: "standard",
        participants: [],
      },
    },
    evidence_excerpts: [],
    related: [relatedRow("mentions"), relatedRow("located_in")],
    cross_corpus: [],
    referenced_atoms: {},
    curation_status: "generated",
  } as unknown as AtomDetailData;
}

describe("AtomDetail related list", () => {
  it("renders without crashing when two related edges share an atom_id (each_key_duplicate regression)", async () => {
    vi.mocked(api.atlasGetAtomDetail).mockResolvedValue(
      detailWithDuplicateNeighbour(),
    );

    render(AtomDetail, {
      props: { corpusId: "wikipedia", atomId: "entity-0255", onBack: () => {} },
    });

    // Header paints once the (mocked, instant) load resolves. If the
    // duplicate key aborted the render, this text never appears and the
    // waitFor times out — which is exactly the user-visible hang.
    await waitFor(() =>
      expect(screen.getByText("2026 Negombo prison riot")).toBeTruthy(),
    );

    // Both related rows survive — the crash would have left zero.
    expect(screen.getAllByText("Repeated Neighbour").length).toBe(2);
  });
});

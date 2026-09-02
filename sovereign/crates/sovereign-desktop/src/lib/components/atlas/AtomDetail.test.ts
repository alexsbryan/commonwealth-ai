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
    // The wire field is `canonical_name` — both producers
    // (`sovereign_mesh::RelatedAtom`, `RelatedAtomDto`) emit that and
    // neither has ever emitted `display_name`. This fixture said
    // `display_name` until 2026-08-21, which is why the assertion below
    // stayed green while the real panel rendered an empty name.
    canonical_name: "Repeated Neighbour",
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

// ─── The author's own nouns, in the inspector ───────────────────

function claimDetail(): AtomDetailData {
  return {
    corpus_id: "wessex-hoard",
    atom_id: "claim-0004",
    stable_key: "sk-4",
    atom_type: "Claim",
    display_name: "struck at Canterbury between 710 and 725",
    atom: {
      atom_type: "Claim",
      data: {
        id: "claim-0004",
        content: "struck at Canterbury between 710 and 725",
        discourse_act: "assert",
        epistemic_status: "confident",
        scope: "particular",
        claim_kind: "attribution",
        // The VOICE and the REFERENT are different atoms and the whole
        // reason a declared claim type names a `subject`.
        attributed_to: "entity-0009",
        subject: "entity-0002",
        attributes: {
          proposed_date: "AH 157 (773 or 774)",
          // A `ref` attribute that resolved, and one that did not.
          mint: "entity-0013",
          basis: "an unidentified continental die-link",
        },
        enrichment_depth: "extracted",
      },
    },
    evidence_excerpts: [],
    related: [],
    cross_corpus: [],
    referenced_atoms: {
      "entity-0002": { display_name: "Wessex Down 2", atom_type: "Entity" },
      "entity-0009": { display_name: "Metcalf", atom_type: "Entity" },
      "entity-0013": { display_name: "Canterbury", atom_type: "Entity" },
    },
    curation_status: "generated",
  } as unknown as AtomDetailData;
}

describe("AtomDetail — a declared Claim", () => {
  it("shows what the claim is ABOUT, distinct from who said it", async () => {
    vi.mocked(api.atlasGetAtomDetail).mockResolvedValue(claimDetail());
    render(AtomDetail, {
      props: { corpusId: "wessex-hoard", atomId: "claim-0004", onBack: () => {} },
    });

    const about = await screen.findByTestId("claim-subject");
    expect(about.textContent).toContain("Wessex Down 2");
    // The voice is still rendered, and it is a different atom.
    expect(screen.getByText("Metcalf")).toBeTruthy();
    // The declared type is named, not left to the generic "Claim" pill.
    expect(
      (await screen.findByTestId("claim-declared-type")).textContent?.trim(),
    ).toBe("attribution");
  });

  it("renders declared attributes as rows, linking only the ones that resolve", async () => {
    vi.mocked(api.atlasGetAtomDetail).mockResolvedValue(claimDetail());
    render(AtomDetail, {
      props: { corpusId: "wessex-hoard", atomId: "claim-0004", onBack: () => {} },
    });

    await screen.findByTestId("atom-attributes");
    const names = screen
      .getAllByTestId("atom-attribute-name")
      .map((n) => n.textContent?.trim());
    expect(names).toEqual(["proposed_date", "mint", "basis"]);

    // A `ref` that names a real atom becomes a link…
    expect(screen.getByText("Canterbury")).toBeTruthy();
    // …and one that is just what the source said stays text, not a
    // chip styled as a broken reference.
    expect(
      screen.getByText("an unidentified continental die-link"),
    ).toBeTruthy();
  });
});

describe("AtomDetail — Position", () => {
  // Before 2026-09-02 the TS `AtomType` union had 8 variants and the
  // backend had 11, so a Position fell off the end of AtomDetail's
  // {#if} chain: EMPTY body, BLANK type pill, no error and no boundary
  // trip. "It renders without a body-render-error" would have passed
  // on that code — this asserts the body's OWN fields instead.
  it("renders its stance, proponent and content", async () => {
    vi.mocked(api.atlasGetAtomDetail).mockResolvedValue({
      corpus_id: "commons",
      atom_id: "position-0001",
      stable_key: "sk-p1",
      atom_type: "Position",
      display_name: "Hardin's tragedy thesis",
      salience: 0.8,
      atom: {
        atom_type: "Position",
        data: {
          id: "position-0001",
          canonical_name: "Hardin's tragedy thesis",
          content:
            "Shared resources are inevitably degraded absent private property or state control.",
          stance: "rebut",
          proponent_id: "entity-0004",
          evidence_ids: [],
          first_appearance: { chunk_id: "sec_0003" },
          anchors: ["tragedy of the commons"],
          salience: 0.8,
          enrichment_depth: "extracted",
        },
      },
      evidence_excerpts: [],
      related: [],
      cross_corpus: [],
      referenced_atoms: {
        "entity-0004": { display_name: "Garrett Hardin", atom_type: "Entity" },
      },
      curation_status: "generated",
    } as unknown as AtomDetailData);

    render(AtomDetail, {
      props: { corpusId: "commons", atomId: "position-0001", onBack: () => {} },
    });

    await screen.findByText(/Shared resources are inevitably degraded/);
    expect(screen.getByText("rebut")).toBeTruthy();
    expect(screen.getByText("Garrett Hardin")).toBeTruthy();
    expect(screen.getByText(/tragedy of the commons/)).toBeTruthy();
    // The header pill names the kind — it was blank while the union
    // was eight wide.
    expect(screen.getAllByText("Position").length).toBeGreaterThan(0);
  });
});


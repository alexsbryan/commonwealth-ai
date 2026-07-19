// SPDX-License-Identifier: AGPL-3.0-or-later
// EpistemicFooter tests (initiative I2-B). The footer is pure props-in:
// it renders the typed epistemic ledger (metadata.epistemic_state) with
// no store, FSM, or Tauri calls. Two layers:
//   - pure-function coverage of the ledger-derivation helpers
//     (one assertion per TurnVerdict + the memory-distinctness rule);
//   - render coverage of the three surfaces (verdict receipt, provenance
//     badges, abstention panel) + the I3 / I6 invariants.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import {
  EpistemicFooter,
  verdictReceipt,
  provKind,
  bandLabel,
  isUnverifiedRecall,
} from "@sovereign/chat-ui";
import type {
  EpistemicState,
  Holding,
  TurnVerdict,
  Verification,
  MemoryBand,
} from "@sovereign/chat-ui";

function ledger(overrides: Partial<EpistemicState> = {}): EpistemicState {
  return {
    version: 1,
    demands: [],
    holdings: [],
    gaps: [],
    verdict: "grounded",
    ...overrides,
  };
}

function corpusHolding(
  verification: Verification = "verified",
  chunk_id: number | null = null,
): Holding {
  return {
    claim: "The knife was a carving knife",
    provenance: { corpus: { corpus_id: "secret-agent", chunk_id } },
    verification,
  };
}

function memoryHolding(
  band: MemoryBand = "told_directly",
  verification: Verification = "verified",
): Holding {
  return {
    claim: "You started a woodworking class in March",
    provenance: { memory: { band, entry_id: "mem-1" } },
    verification,
  };
}

describe("ledger-derivation helpers", () => {
  it("provKind collapses each provenance variant to its group", () => {
    expect(provKind({ corpus: { corpus_id: null, chunk_id: null } })).toBe("corpus");
    expect(provKind({ memory: { band: "inferred", entry_id: "x" } })).toBe("memory");
    expect(provKind("general_knowledge")).toBe("general_knowledge");
    expect(provKind({ tool_derived: { tool: "parcel_analytics" } })).toBe(
      "tool_derived",
    );
  });

  it("bandLabel maps each memory band to human language", () => {
    expect(bandLabel("told_directly")).toBe("what you told me");
    expect(bandLabel("inferred")).toBe("inferred");
    expect(bandLabel("tentative")).toBe("tentative");
  });

  it("isUnverifiedRecall is true only for memory holdings that shipped unchecked", () => {
    expect(isUnverifiedRecall(memoryHolding("told_directly", "fail_open"))).toBe(true);
    expect(isUnverifiedRecall(memoryHolding("tentative", "unverified"))).toBe(true);
    expect(isUnverifiedRecall(memoryHolding("told_directly", "verified"))).toBe(false);
    // A corpus holding is never a "remembered, not verified" case.
    expect(isUnverifiedRecall(corpusHolding("unverified"))).toBe(false);
  });

  it("verdictReceipt derives one receipt per verdict (never model-asserted)", () => {
    const cases: Array<[TurnVerdict, string | null]> = [
      ["grounded", "grounded"],
      ["mixed", "neutral"],
      ["memory_recall", "neutral"],
      ["general_knowledge", "caution"],
      ["unverified", "caution"],
      ["cannot_know_from_here", null], // abstention panel owns it
    ];
    for (const [verdict, tone] of cases) {
      const r = verdictReceipt(ledger({ verdict }));
      if (tone === null) {
        expect(r, `${verdict} yields no receipt`).toBeNull();
      } else {
        expect(r?.tone, `${verdict} tone`).toBe(tone);
        expect(r?.text.length, `${verdict} text`).toBeGreaterThan(0);
      }
    }
  });

  it("grounded receipt counts verified corpus claims", () => {
    const r = verdictReceipt(
      ledger({
        verdict: "grounded",
        holdings: [corpusHolding("verified"), corpusHolding("verified")],
      }),
    );
    expect(r?.text).toContain("2 claims checked");
  });
});

describe("EpistemicFooter render", () => {
  it("renders the footer with a data-verdict hook", () => {
    const { container } = render(EpistemicFooter, {
      props: { ledger: ledger({ verdict: "grounded", holdings: [corpusHolding()] }) },
    });
    const footer = container.querySelector('[data-testid="epistemic-footer"]');
    expect(footer).not.toBeNull();
    expect(footer?.getAttribute("data-verdict")).toBe("grounded");
  });

  it("groups holdings into provenance badges", () => {
    render(EpistemicFooter, {
      props: {
        ledger: ledger({
          verdict: "mixed",
          holdings: [corpusHolding(), memoryHolding()],
        }),
      },
    });
    expect(screen.getByText("Sources (1)")).toBeInTheDocument();
    expect(screen.getByText("Memory (1)")).toBeInTheDocument();
  });

  it("I3: memory renders distinctly — band label + 'remembered, not verified'", async () => {
    const { container } = render(EpistemicFooter, {
      props: {
        ledger: ledger({
          verdict: "memory_recall",
          holdings: [memoryHolding("told_directly", "fail_open")],
        }),
      },
    });
    // Expand the holdings list.
    await fireEvent.click(container.querySelector(".badges")!);
    expect(screen.getByText("what you told me")).toBeInTheDocument();
    expect(screen.getByText("remembered, not verified")).toBeInTheDocument();
    // Memory is never labelled as a document source.
    expect(screen.queryByText("Sources (1)")).toBeNull();
  });

  it("cannot_know_from_here renders the abstention panel, not a receipt", () => {
    const { container } = render(EpistemicFooter, {
      props: {
        ledger: ledger({
          verdict: "cannot_know_from_here",
          gaps: [
            {
              demand_idx: 0,
              statement: "Your sources didn't settle this question",
              coverage: "topic_uncovered",
              routes: [],
            },
          ],
        }),
      },
    });
    expect(
      screen.getByText("Not answerable from your current sources"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Your sources didn't settle this question"),
    ).toBeInTheDocument();
    // The verdict receipt must NOT render on an abstention turn.
    expect(
      container.querySelector('[data-testid="epistemic-receipt"]'),
    ).toBeNull();
  });

  it("abstention route chips navigate to the Library and gate on onOpenLibrary", async () => {
    const onOpenLibrary = vi.fn();
    const gaps = [
      {
        demand_idx: 0,
        statement: "No source material found",
        coverage: "topic_uncovered" as const,
        routes: [
          { install_recipe: { recipe_id: "sep", name: "Philosophy (SEP)" } },
          "connect_folder" as const,
        ],
      },
    ];
    const { rerender, container } = render(EpistemicFooter, {
      props: { ledger: ledger({ verdict: "cannot_know_from_here", gaps }) },
    });
    // Without onOpenLibrary, no route chips render (CLI/test hosts).
    expect(
      container.querySelector('[data-testid="abstention-routes"]'),
    ).toBeNull();

    // With the callback, chips render and click navigates.
    await rerender({
      ledger: ledger({ verdict: "cannot_know_from_here", gaps }),
      onOpenLibrary,
    });
    const install = screen.getByText("Install Philosophy (SEP)");
    expect(install).toBeInTheDocument();
    expect(screen.getByText("Connect a folder")).toBeInTheDocument();
    await fireEvent.click(install);
    expect(onOpenLibrary).toHaveBeenCalledTimes(1);
  });

  it("I6: renders purely from the ledger object (prose can't override it)", () => {
    // The footer takes no prose — its only input is the typed ledger, so
    // a contradicting answer body structurally cannot change what renders.
    const { container } = render(EpistemicFooter, {
      props: { ledger: ledger({ verdict: "general_knowledge" }) },
    });
    const footer = container.querySelector('[data-testid="epistemic-footer"]');
    expect(footer?.getAttribute("data-verdict")).toBe("general_knowledge");
    expect(screen.getByText(/From general knowledge/)).toBeInTheDocument();
  });
});

// SPDX-License-Identifier: AGPL-3.0-or-later
// CoverageCard — the render side of FINANCIAL_CORPORA §7.7 (F5/F6).
//
// §7.7 is settled design, not taste, so these are contract tests rather
// than smoke tests. Each one pins a rule the spec states, and each has a
// failing input you can name:
//
//   §7.7(1) a refusal is never styled as a failure  -> no error/warn hook
//   §7.7(2) boundaries at EQUAL weight              -> same classes, no <details>
//   §7.7(3) content derived, never authored         -> second filer, no new copy
//   §7.7(5) as-of always shown                      -> present in every render
//   order:  never render a percentage               -> no "%" anywhere
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import CoverageCard from "./CoverageCard.svelte";
import type { CoverageCard as CoverageCardData } from "../../types";

vi.mock("../../api", () => ({
  corpusCoverageCard: vi.fn(async () => null),
}));

const api = await import("../../api");

/** Apple-shaped card, as `coverage_card()` derives it. */
function appleCard(): CoverageCardData {
  return {
    entity: "Apple Inc.",
    ticker: "AAPL",
    cik: "0000320193",
    period_label: "FY2015-FY2025",
    answers: [
      {
        id: "revenue",
        label: "Total revenue (net sales)",
        kind: "duration",
        period_label: "FY2024-FY2025",
        fiscal_years: [2024, 2025],
      },
      {
        id: "total_assets",
        label: "Total assets",
        kind: "instant",
        period_label: "FY2025",
        fiscal_years: [2025],
      },
    ],
    limits: [
      {
        kind: "consolidated",
        statement:
          "SEC companyfacts is consolidated-only: figures broken out by segment or other dimension — revenue for a single business segment, for example — are not typed here.",
      },
      {
        kind: "untyped_tags",
        statement:
          "479 further XBRL tags this filer reports are not typed yet. They are listed by name in the corpus's _unmapped_concepts.json.",
      },
      {
        kind: "beyond_as_of",
        statement:
          "No figure exists for any period ending after 2025-09-27. This corpus is exactly as current as the 10-K filed 2025-10-31.",
      },
    ],
    as_of: {
      form: "10-K",
      accession: "0000320193-25-000079",
      filed: "2025-10-31",
      latest_period_end: "2025-09-27",
    },
  };
}

/** A second filer — different everything, and NO consolidated/untyped
 *  limits. Proves the render is derived rather than written for Apple. */
function contosoCard(): CoverageCardData {
  return {
    entity: "Contoso Pharmaceuticals PLC",
    ticker: "CTSO",
    cik: "0000999999",
    period_label: "FY2022-FY2023",
    answers: [
      {
        id: "research_and_development",
        label: "Research and development expense",
        kind: "duration",
        period_label: "FY2022-FY2023",
        fiscal_years: [2022, 2023],
      },
    ],
    limits: [
      {
        kind: "beyond_as_of",
        statement:
          "No figure exists for any period ending after 2023-12-31. This corpus is exactly as current as the 20-F filed 2024-03-15.",
      },
    ],
    as_of: {
      form: "20-F",
      accession: "0000999999-24-000001",
      filed: "2024-03-15",
      latest_period_end: "2023-12-31",
    },
  };
}

async function renderCard(card: CoverageCardData | null) {
  vi.mocked(api.corpusCoverageCard).mockResolvedValue(card);
  render(CoverageCard, { props: { corpusId: "sec-cik0000320193" } });
  if (card) await screen.findByTestId("coverage-card");
  return card;
}

describe("CoverageCard", () => {
  beforeEach(() => {
    vi.mocked(api.corpusCoverageCard).mockClear();
  });

  it("renders nothing for a corpus with no typed store", async () => {
    vi.mocked(api.corpusCoverageCard).mockResolvedValue(null);
    render(CoverageCard, { props: { corpusId: "wikipedia" } });
    await waitFor(() =>
      expect(api.corpusCoverageCard).toHaveBeenCalledWith("wikipedia"),
    );
    expect(screen.queryByTestId("coverage-card")).not.toBeInTheDocument();
  });

  it("leads with what the corpus answers, naming concept and period", async () => {
    await renderCard(appleCard());
    const answers = screen.getByTestId("coverage-answers");
    expect(answers).toHaveTextContent("Total revenue (net sales)");
    expect(answers).toHaveTextContent("FY2024-FY2025");
    expect(answers).toHaveTextContent("Total assets");
  });

  // §7.7(2). Capability and boundaries must render at equal weight — not
  // fine print, not a smaller type ramp, not a collapsed disclosure. The
  // component holds this by giving both sections the same classes, so
  // this asserts on the class lists rather than on computed styles
  // (jsdom does not apply a <style> block).
  it("states boundaries at the same weight as capability", async () => {
    await renderCard(appleCard());
    const answers = screen.getByTestId("coverage-answers");
    const limits = screen.getByTestId("coverage-limits");

    expect(answers.className).toContain("cc-section");
    expect(limits.className).toContain("cc-section");
    expect(limits.className).toBe(answers.className);

    // Both headings are the same element type — a boundary is not
    // demoted a level below the capability it sits beside.
    expect(answers.querySelector("h3")).not.toBeNull();
    expect(limits.querySelector("h3")).not.toBeNull();

    // Every limit renders as a `.cc-fact`, the same rule the capability
    // list uses.
    const limitItems = limits.querySelectorAll("li");
    expect(limitItems.length).toBe(3);
    limitItems.forEach((li) => expect(li.className).toContain("cc-fact"));

    // Not behind a disclosure.
    expect(limits.querySelector("details")).toBeNull();
    expect(limits.querySelector("summary")).toBeNull();
  });

  // §7.7(1). A refusal is a correct answer and is never styled as a
  // failure: no error/warning class, no alert role, no apology.
  it("never styles a boundary as a fault", async () => {
    await renderCard(appleCard());
    const card = screen.getByTestId("coverage-card");

    expect(card.querySelector('[role="alert"]')).toBeNull();
    expect(card.querySelector('[role="status"]')).toBeNull();
    const html = card.innerHTML;
    for (const hook of ["error", "warning", "warn", "danger", "alert"]) {
      expect(html.toLowerCase()).not.toContain(`class="${hook}`);
      expect(html.toLowerCase()).not.toContain(`${hook}-`);
    }
    for (const apology of ["sorry", "unfortunately", "failed", "problem"]) {
      expect(card.textContent?.toLowerCase()).not.toContain(apology);
    }
  });

  // The order's explicit prohibition: 24 of 503 tags mapped must never
  // render as "5% coverage". Nothing gives the renderer a ratio, and it
  // must not invent one.
  it("renders no percentage anywhere", async () => {
    await renderCard(appleCard());
    const card = screen.getByTestId("coverage-card");
    expect(card.textContent).not.toContain("%");
    expect(card.textContent).not.toMatch(/\bcoverage\s*[:=]?\s*\d/i);
  });

  // §7.7(5) / F6.
  it("always shows the as-of filing", async () => {
    await renderCard(appleCard());
    const asOf = screen.getByTestId("coverage-as-of");
    expect(asOf).toHaveTextContent("10-K");
    expect(asOf).toHaveTextContent("2025-10-31");
    expect(asOf).toHaveTextContent("0000320193-25-000079");
    expect(asOf).toHaveTextContent("2025-09-27");
  });

  // §7.7(3) and the order's F5/F6 bar: a second corpus gets a truthful
  // card with NO new copy written. The same component, given another
  // filer's derived card, renders that filer's facts and only the limits
  // that filer actually has.
  it("renders a second filer truthfully with no new copy", async () => {
    await renderCard(contosoCard());
    const card = screen.getByTestId("coverage-card");

    expect(card).toHaveTextContent("Research and development expense");
    expect(card).toHaveTextContent("FY2022-FY2023");
    expect(screen.getByTestId("coverage-as-of")).toHaveTextContent("20-F");
    expect(screen.getByTestId("coverage-as-of")).toHaveTextContent(
      "2024-03-15",
    );

    // No Apple string survives — nothing in the component is written for
    // one company.
    for (const appleism of ["Apple", "AAPL", "10-K", "Services", "0000320193"]) {
      expect(card.textContent).not.toContain(appleism);
    }

    // This store has no consolidated or untyped-tag limit, so the card
    // must not assert one it does not have.
    const limits = screen.getByTestId("coverage-limits");
    expect(limits.querySelectorAll("li").length).toBe(1);
    expect(
      limits.querySelector('[data-limit-kind="consolidated"]'),
    ).toBeNull();
    expect(limits.querySelector('[data-limit-kind="untyped_tags"]')).toBeNull();
    expect(
      limits.querySelector('[data-limit-kind="beyond_as_of"]'),
    ).not.toBeNull();
  });
});

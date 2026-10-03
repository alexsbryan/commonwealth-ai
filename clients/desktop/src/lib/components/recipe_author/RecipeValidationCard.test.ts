// SPDX-License-Identifier: AGPL-3.0-or-later
// RecipeValidationCard — the three lists are three different things, and the
// card must not collapse them. `errors` block the build, `warnings` do not,
// and `notes` are the DERIVED facets (clock, tension selector, identity
// criterion, question shapes) that `ONTOLOGY_PRIMITIVES.md` §6 says an author
// must be able to see and override. Props-in pattern, as LessonCard.test.ts.
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/svelte";
import RecipeValidationCard from "./RecipeValidationCard.svelte";
import type { RecipeValidationReport } from "../../types";

function report(
  overrides: Partial<RecipeValidationReport> = {},
): RecipeValidationReport {
  return {
    ok: true,
    errors: [],
    no_recipe: false,
    enrichment_ready: true,
    warnings: [],
    notes: [],
    ...overrides,
  };
}

describe("RecipeValidationCard", () => {
  it("shows the valid pill and no warning/derived sections when both lists are empty", () => {
    render(RecipeValidationCard, { props: { validation: report() } });
    expect(screen.getByText("valid")).toBeInTheDocument();
    expect(
      screen.queryByTestId("recipe-validation-warnings"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByTestId("recipe-validation-notes"),
    ).not.toBeInTheDocument();
  });

  it("renders derived facets under a valid recipe, as notes and not as warnings", () => {
    render(RecipeValidationCard, {
      props: {
        validation: report({
          notes: [
            "clock: document_date — supersession folds on document dates",
            "identity: coin → canonical name (default)",
          ],
        }),
      },
    });
    expect(screen.getByText("valid")).toBeInTheDocument();
    expect(screen.getByTestId("recipe-validation-derived-pill")).toBeInTheDocument();
    expect(
      screen.getByText(/identity: coin → canonical name/),
    ).toBeInTheDocument();
    // A facet must never be counted as a defect.
    expect(
      screen.queryByTestId("recipe-validation-warning-pill"),
    ).not.toBeInTheDocument();
  });

  it("counts warnings separately from errors and keeps the recipe valid", () => {
    render(RecipeValidationCard, {
      props: {
        validation: report({
          warnings: ["`corpus.license` is empty — add a SPDX identifier"],
          notes: ["clock: none — nothing supersedes"],
        }),
      },
    });
    expect(screen.getByText("valid")).toBeInTheDocument();
    expect(screen.getByText("1 warning")).toBeInTheDocument();
    expect(screen.getByText(/corpus.license` is empty/)).toBeInTheDocument();
    expect(screen.getByTestId("recipe-validation-notes")).toBeInTheDocument();
  });

  it("shows a semantic error as blocking, with the fix action", () => {
    render(RecipeValidationCard, {
      props: {
        validation: report({
          ok: false,
          enrichment_ready: false,
          errors: [
            'ontology type `ruler`: `role_of = "mint"` does not name a declared type',
          ],
          warnings: ["`corpus.license` is empty — add a SPDX identifier"],
        }),
      },
    });
    expect(screen.getByText("needs attention")).toBeInTheDocument();
    expect(screen.getByText(/1 issue blocking the recipe/)).toBeInTheDocument();
    expect(
      screen.getByTestId("recipe-validation-ask-fix"),
    ).toBeInTheDocument();
    // Warnings still render alongside a blocking error — they are not swallowed.
    expect(screen.getByText("1 warning")).toBeInTheDocument();
  });

  it("says nothing at all when there is no recipe yet", () => {
    render(RecipeValidationCard, {
      props: {
        validation: report({
          ok: false,
          no_recipe: true,
          enrichment_ready: false,
          warnings: ["would be hidden"],
          notes: ["would be hidden"],
        }),
      },
    });
    expect(screen.getByText("No recipe drafted yet.")).toBeInTheDocument();
    expect(
      screen.queryByTestId("recipe-validation-warnings"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByTestId("recipe-validation-notes"),
    ).not.toBeInTheDocument();
  });
});

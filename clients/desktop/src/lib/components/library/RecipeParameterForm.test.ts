// SPDX-License-Identifier: AGPL-3.0-or-later
// RecipeParameterForm — the install-time form for a parameterized recipe.
//
// The rule under test is FINANCIAL_CORPORA §7.1's "no new install UI":
// the form is a function of the recipe's own `[parameters]` schema, so a
// DIFFERENT recipe must get a correct form with no new code. Every test
// here names its failing input:
//
//   schema-driven      -> a recipe with no ticker at all still renders
//   kind drives control-> `int`/`date`/`list` are not all text boxes
//   defaults visible   -> `contact` is shown and editable, not hidden
//   required gate      -> blank ticker cannot reach the daemon
//   blank optional     -> omitted, so the recipe's default stands
//   refusal reported   -> a daemon 400 is rendered, not console-swallowed
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/svelte";
import RecipeParameterForm from "./RecipeParameterForm.svelte";
import type { RecipeParameter } from "../../api";

vi.mock("../../api", () => ({
  corpusGetRecipeParameters: vi.fn(async () => ({
    corpus_id: "x",
    parameters: [],
  })),
  corpusInstallWithParameters: vi.fn(async () => undefined),
}));

const api = await import("../../api");

/** `sec-filings-company`'s declared block, verbatim in shape. */
function secParameters(): RecipeParameter[] {
  return [
    {
      name: "ticker",
      kind: "string",
      description: "Stock ticker of the company to install, e.g. AAPL.",
      required: true,
      default: null,
    },
    {
      name: "contact",
      kind: "string",
      description: "Contact address sent to SEC in the User-Agent.",
      required: false,
      default: "alexbryan01@gmail.com",
    },
  ];
}

function mockSchema(corpusId: string, parameters: RecipeParameter[]) {
  vi.mocked(api.corpusGetRecipeParameters).mockResolvedValue({
    corpus_id: corpusId,
    parameters,
  });
}

function mount(corpusId = "sec-filings-company") {
  return render(RecipeParameterForm, {
    props: {
      corpusId,
      corpusName: "SEC Filings — Single Company",
      onInstalled: vi.fn(),
      onCancel: vi.fn(),
    },
  });
}

describe("RecipeParameterForm", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.corpusInstallWithParameters).mockResolvedValue(undefined);
  });

  it("renders one control per declared parameter, from the schema alone", async () => {
    mockSchema("sec-filings-company", secParameters());
    mount();
    await waitFor(() => expect(screen.getByTestId("param-ticker")).toBeTruthy());
    expect(screen.getByTestId("param-contact")).toBeTruthy();
  });

  it("renders a DIFFERENT recipe's parameters with no code specific to it", async () => {
    // The authored-content failure this guards: hand-coding a ticker
    // input would make every other parameterized recipe render wrong.
    mockSchema("sf-assessor-roll", [
      { name: "roll_year", kind: "int", description: "Assessment roll year.", required: true, default: null },
      { name: "as_of", kind: "date", description: "Snapshot date.", required: false, default: null },
      { name: "neighborhoods", kind: "list", description: "Codes to include.", required: false, default: null },
    ]);
    mount("sf-assessor-roll");
    await waitFor(() => expect(screen.getByTestId("param-roll_year")).toBeTruthy());
    // `kind` drives the affordance (SCHEMA.md:50) — not all text boxes.
    expect(screen.getByTestId("param-roll_year").getAttribute("type")).toBe("number");
    expect(screen.getByTestId("param-as_of").getAttribute("type")).toBe("date");
    expect(screen.getByTestId("param-neighborhoods").getAttribute("placeholder")).toBe(
      "comma-separated",
    );
    // And nothing from the SEC recipe leaked in.
    expect(screen.queryByTestId("param-ticker")).toBeNull();
  });

  it("shows a defaulted parameter pre-filled and editable rather than hidden", async () => {
    // `contact` is the address SEC is told to reach the user at. The
    // recipe made it a parameter instead of a constant for this reason.
    mockSchema("sec-filings-company", secParameters());
    mount();
    const contact = (await waitFor(() =>
      screen.getByTestId("param-contact"),
    )) as HTMLInputElement;
    expect(contact.value).toBe("alexbryan01@gmail.com");
    expect(contact.disabled).toBe(false);
    expect(contact.readOnly).toBe(false);
  });

  it("refuses to install until every required parameter has a value", async () => {
    mockSchema("sec-filings-company", secParameters());
    mount();
    const install = (await waitFor(() =>
      screen.getByTestId("param-form-install"),
    )) as HTMLButtonElement;
    // The named failing input: a blank ticker, which would reach the
    // acquirer as an empty string and refuse there instead of here.
    expect(install.disabled).toBe(true);
    expect(screen.getByTestId("param-form-missing").textContent).toContain("ticker");
    expect(api.corpusInstallWithParameters).not.toHaveBeenCalled();
  });

  it("sends the typed values, omitting a blanked optional so the recipe default stands", async () => {
    mockSchema("sec-filings-company", secParameters());
    mount();
    const ticker = (await waitFor(() => screen.getByTestId("param-ticker"))) as HTMLInputElement;
    await fireEvent.input(ticker, { target: { value: "AAPL" } });
    const contact = screen.getByTestId("param-contact") as HTMLInputElement;
    await fireEvent.input(contact, { target: { value: "  " } });

    const install = screen.getByTestId("param-form-install") as HTMLButtonElement;
    await waitFor(() => expect(install.disabled).toBe(false));
    await fireEvent.click(install);

    await waitFor(() =>
      expect(api.corpusInstallWithParameters).toHaveBeenCalledWith("sec-filings-company", {
        ticker: "AAPL",
      }),
    );
  });

  it("converts an int parameter to a number and a list to an array", async () => {
    mockSchema("sf-assessor-roll", [
      { name: "roll_year", kind: "int", description: "Year.", required: true, default: null },
      { name: "neighborhoods", kind: "list", description: "Codes.", required: false, default: null },
    ]);
    mount("sf-assessor-roll");
    const year = (await waitFor(() => screen.getByTestId("param-roll_year"))) as HTMLInputElement;
    await fireEvent.input(year, { target: { value: "2025" } });
    await fireEvent.input(screen.getByTestId("param-neighborhoods"), {
      target: { value: "01A, 02B , " },
    });
    await fireEvent.click(screen.getByTestId("param-form-install"));

    await waitFor(() =>
      expect(api.corpusInstallWithParameters).toHaveBeenCalledWith("sf-assessor-roll", {
        roll_year: 2025,
        neighborhoods: ["01A", "02B"],
      }),
    );
  });

  it("renders the daemon's refusal instead of swallowing it to the console", async () => {
    // The install path's 400 names the offending parameter; the plain
    // install path only console.error's it, which is a silent failure
    // from where the user is standing (ARCH §18.3).
    mockSchema("sec-filings-company", secParameters());
    vi.mocked(api.corpusInstallWithParameters).mockRejectedValue(
      "invalid parameters for 'sec-filings-company': ticker must be a string",
    );
    const onInstalled = vi.fn();
    render(RecipeParameterForm, {
      props: {
        corpusId: "sec-filings-company",
        corpusName: "SEC Filings",
        onInstalled,
        onCancel: vi.fn(),
      },
    });
    const ticker = (await waitFor(() => screen.getByTestId("param-ticker"))) as HTMLInputElement;
    await fireEvent.input(ticker, { target: { value: "AAPL" } });
    await fireEvent.click(screen.getByTestId("param-form-install"));

    await waitFor(() =>
      expect(screen.getByTestId("param-form-error").textContent).toContain(
        "invalid parameters",
      ),
    );
    expect(onInstalled).not.toHaveBeenCalled();
  });

  it("reports an unreadable schema rather than installing without parameters", async () => {
    vi.mocked(api.corpusGetRecipeParameters).mockRejectedValue("no such recipe");
    mount("ghost-recipe");
    await waitFor(() =>
      expect(screen.getByTestId("param-form-error").textContent).toContain(
        "Could not read this recipe's parameters",
      ),
    );
    expect(api.corpusInstallWithParameters).not.toHaveBeenCalled();
  });
});

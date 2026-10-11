// SPDX-License-Identifier: AGPL-3.0-or-later
// A verified quote opens where it stands: the store reads the stored text at
// the quote's address through the `read_text_slice` command (the daemon's
// `GET {text_endpoint}/{sha}`, OICP v0.5 §2.2) — never through a chunk
// window — and checks the slice against the quote's own `exact`.
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { QuoteAddress } from "../components/answerProvenance";
import type { TextSlice } from "./readingSession.svelte";

vi.mock("../invoke", () => ({ invoke: vi.fn() }));

const { readingSession, QUOTE_CONTEXT } = await import("./readingSession.svelte");
const { invoke } = await import("../invoke");
const fake = vi.mocked(invoke);

const SHA = "fc119db33120843e339028e353a38bcb6eabf74833e5714d5f722b502fb015a2";
const quote: QuoteAddress = {
  corpusId: "oicp-conformance-fixture",
  textSha256: SHA,
  start: 56,
  end: 164,
  exact:
    "No single ministry can speak for the water, and no treaty has yet persuaded a flood to wait for a signature.",
};

/** What a v0.5 host answers for that range: the spec's `TextSlice`. */
function slice(text: string): TextSlice {
  return {
    document: {
      text_sha256: SHA,
      extractor: "plain_text@test",
      source: { id: "okafor2019", sha256: SHA },
      metadata: { title: "Shared Gauges and the Dry Season" },
    },
    start: quote.start,
    end: quote.end,
    text,
    before: "borders keep their own counsel. ",
    after: "\n\nThe basin agreements",
  };
}

describe("readingSession.openQuote", () => {
  beforeEach(() => {
    fake.mockReset();
    readingSession.closeReading();
  });

  it("reads the text at the quote's address, not a chunk window", async () => {
    fake.mockResolvedValue(slice(quote.exact));
    await readingSession.openQuote(quote, "what did the survey find?");
    expect(fake).toHaveBeenCalledTimes(1);
    expect(fake).toHaveBeenCalledWith("read_text_slice", {
      textSha256: SHA,
      start: 56,
      end: 164,
      context: QUOTE_CONTEXT,
      corpus: "oicp-conformance-fixture",
    });
    expect(readingSession.isOpen).toBe(true);
    expect(readingSession.currentReading).toBeNull();
    expect(readingSession.currentText?.matches).toBe(true);
    expect(readingSession.trail.map((s) => s.kind)).toEqual(["question", "quote"]);
    expect(readingSession.trail[1].label).toBe("Shared Gauges and the Dry Season");
  });

  it("keeps a slice that no longer reads as quoted visible as a mismatch", async () => {
    fake.mockResolvedValue(slice(quote.exact.replace("persuaded", "convinced")));
    await readingSession.openQuote(quote, "q");
    expect(readingSession.currentText?.matches).toBe(false);
  });

  it("names a refusal by its published reason and opens nothing", async () => {
    fake.mockRejectedValue(new Error("text not held"));
    await readingSession.openQuote(quote, "q");
    expect(readingSession.isOpen).toBe(false);
    expect(readingSession.error).toContain("text not held");
  });
});

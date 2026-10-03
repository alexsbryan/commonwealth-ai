// SPDX-License-Identifier: AGPL-3.0-or-later
// Tests for the glassbox ETA helpers. The ETA is derived from the backend's
// real embed throughput (chunks_per_sec + chunks_total forwarded from
// IngestProgress::Embedding), NOT a fabricated client guess — so the honest
// contract is: no rate or no total ⇒ no estimate (null ⇒ "—"), never a made-up
// number.
import { describe, it, expect } from "vitest";
import {
  etaSecondsFor,
  formatEta,
  isTerminalPhase,
  selfPrunes,
  shouldStore,
} from "./corpusProgress.svelte";
import type { CorpusInstallPhase, CorpusProgressPayload } from "../types";

function payload(over: Partial<CorpusProgressPayload> = {}): CorpusProgressPayload {
  return {
    corpus_id: "obsidian-vault-abc",
    phase: "embedding",
    percent: 50,
    chunks_processed: 1000,
    chunks_total: 2000,
    chunks_per_sec: 50,
    ...over,
  };
}

describe("etaSecondsFor", () => {
  it("computes remaining/rate from backend throughput", () => {
    // (2000 - 1000) / 50 = 20s
    expect(etaSecondsFor(payload())).toBe(20);
  });

  it("returns null when there is no live rate (can't honestly estimate)", () => {
    expect(etaSecondsFor(payload({ chunks_per_sec: 0 }))).toBeNull();
    expect(etaSecondsFor(payload({ chunks_per_sec: undefined }))).toBeNull();
  });

  it("returns null when the total is unknown", () => {
    expect(etaSecondsFor(payload({ chunks_total: 0 }))).toBeNull();
    expect(etaSecondsFor(payload({ chunks_total: undefined }))).toBeNull();
  });

  it("returns 0 when already past the total (no negative ETA)", () => {
    expect(etaSecondsFor(payload({ chunks_processed: 2500 }))).toBe(0);
  });

  it("returns null for an absent payload", () => {
    expect(etaSecondsFor(undefined)).toBeNull();
  });
});

describe("formatEta", () => {
  it("renders null as an em-dash (no estimate)", () => {
    expect(formatEta(null)).toBe("—");
  });
  it("renders sub-90s as seconds", () => {
    expect(formatEta(20)).toBe("~20s");
    expect(formatEta(89)).toBe("~89s");
  });
  it("renders minutes for the mid range", () => {
    expect(formatEta(240)).toBe("~4 min");
    expect(formatEta(90)).toBe("~2 min");
  });
  it("renders hours past 90 minutes", () => {
    expect(formatEta(3600 * 2)).toBe("~2.0 h");
    expect(formatEta(3600 * 12)).toBe("~12 h");
  });
  it("renders a near-zero ETA as 'almost done'", () => {
    expect(formatEta(0)).toBe("almost done");
  });
  it("always marks the estimate as approximate with a ~", () => {
    expect(formatEta(45).startsWith("~")).toBe(true);
    expect(formatEta(300).startsWith("~")).toBe(true);
  });
});

// The retention policy for terminal phases. This is not cosmetic: a
// failed install used to be reported as "Done" at 100% (the daemon only
// logged the error, the corpus dropped out of the status snapshot, and
// the poller read the disappearance as success). Now that a failure
// actually reaches the frontend, it has to STAY long enough to be read
// and acted on — an 800 ms flash of the reason is no better than the
// silence it replaced.
describe("terminal phase retention", () => {
  it("treats complete and failed as the two terminal phases", () => {
    expect(isTerminalPhase("complete")).toBe(true);
    expect(isTerminalPhase("failed")).toBe(true);
  });

  it("treats every working phase as non-terminal", () => {
    for (const phase of [
      "downloading",
      "extracting",
      "chunking",
      "embedding",
      "indexing",
      "optimizing_index",
      "enriching_clustering",
    ]) {
      expect(isTerminalPhase(phase)).toBe(false);
    }
  });

  it("self-prunes a completed install so the row gets out of the way", () => {
    expect(selfPrunes("complete")).toBe(true);
  });

  it("does NOT self-prune a failure — it must persist until acted on", () => {
    expect(selfPrunes("failed")).toBe(false);
  });

  it("never self-prunes a still-running install", () => {
    expect(selfPrunes("downloading")).toBe(false);
    expect(selfPrunes("embedding")).toBe(false);
  });

  it("only ever self-prunes something terminal", () => {
    for (const phase of ["complete", "failed", "downloading", "embedding"]) {
      if (selfPrunes(phase)) expect(isTerminalPhase(phase)).toBe(true);
    }
  });
});

// Dismissal has to be sticky against REPETITION, because a failure is
// sticky on the daemon side: the record survives in `corpus_progress`
// until a retry sweeps it, so the desktop's status poller re-emits the
// identical `failed` payload once a second. A naive dismiss (just delete
// the entry) is undone within one tick — a button that does nothing.
describe("shouldStore / dismissal", () => {
  it("stores a failure the user has not dismissed", () => {
    const dismissed = new Set<string>();
    expect(shouldStore({ corpus_id: "sep", phase: "failed" }, dismissed)).toBe(true);
  });

  it("suppresses every repeat of a dismissed failure", () => {
    const dismissed = new Set(["sep"]);
    // The poller repeats once a second; all of them must stay suppressed.
    for (let i = 0; i < 5; i++) {
      expect(shouldStore({ corpus_id: "sep", phase: "failed" }, dismissed)).toBe(false);
    }
  });

  it("only suppresses the dismissed corpus, not its neighbours", () => {
    const dismissed = new Set(["sep"]);
    expect(shouldStore({ corpus_id: "wikipedia", phase: "failed" }, dismissed)).toBe(true);
  });

  it("revives on a retry, so a NEW failure is shown again", () => {
    const dismissed = new Set(["sep"]);
    // A retry starts: the first non-failed payload clears the dismissal…
    expect(shouldStore({ corpus_id: "sep", phase: "downloading" }, dismissed)).toBe(true);
    expect(dismissed.has("sep")).toBe(false);
    // …so if the retry also fails, the user sees it rather than having it
    // swallowed by their earlier acknowledgement.
    expect(shouldStore({ corpus_id: "sep", phase: "failed" }, dismissed)).toBe(true);
  });

  it("never suppresses a non-failed phase", () => {
    const dismissed = new Set(["sep"]);
    const phases: CorpusInstallPhase[] = ["downloading", "embedding", "complete"];
    for (const phase of phases) {
      expect(shouldStore({ corpus_id: "sep", phase }, dismissed)).toBe(true);
    }
  });
});

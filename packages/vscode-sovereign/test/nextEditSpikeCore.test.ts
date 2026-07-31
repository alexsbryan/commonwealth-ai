import { describe, expect, it } from "vitest";
import {
  chooseScenario,
  findSites,
  findWordSites,
} from "../src/nextEditSpikeCore";

describe("findSites", () => {
  it("orders sites after the cursor first, then wraps", () => {
    const text = "log a\nlog b\nlog c\n";
    // cursor between the first and second occurrence
    expect(findSites(text, "log", 3)).toEqual([6, 12, 0]);
  });

  it("scans past each match so applications never overlap", () => {
    expect(findSites("aaaa", "aa", 0)).toEqual([0, 2]);
  });

  it("returns empty for an absent needle or empty find", () => {
    expect(findSites("abc", "z", 0)).toEqual([]);
    expect(findSites("abc", "", 0)).toEqual([]);
  });
});

describe("findWordSites", () => {
  it("excludes substring matches inside identifiers", () => {
    const text = "word wordNext word_tail my.word";
    // offsets: 0 ok, 5 inside wordNext, 14 has _ after, 27 ok
    expect(findWordSites(text, "word", 0)).toEqual([0, 27]);
  });

  it("does not re-match its own rename output", () => {
    const renamed = "wordNext wordNext";
    expect(findWordSites(renamed, "word", 0)).toEqual([]);
  });
});

describe("chooseScenario", () => {
  it("prefers the console.log scenario when present", () => {
    const s = chooseScenario('console.log("x"); word word', "word");
    expect(s?.rule).toEqual({
      find: "console.log(",
      replace: "console.debug(",
    });
    expect(s?.wholeWord).toBe(false);
  });

  it("falls back to word rename when the word repeats", () => {
    const s = chooseScenario("word other word", "word");
    expect(s?.rule).toEqual({ find: "word", replace: "wordNext" });
    expect(s?.wholeWord).toBe(true);
  });

  it("declines a word that appears only once, or no word", () => {
    expect(chooseScenario("word once", "word")).toBeNull();
    expect(chooseScenario("nothing here", null)).toBeNull();
  });
});

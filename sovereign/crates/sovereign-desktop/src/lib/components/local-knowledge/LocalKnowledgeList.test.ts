// SPDX-License-Identifier: AGPL-3.0-or-later
// LocalKnowledgeList label tests. Regression guard for the bug where every
// non-DocumentFolder source (including watched folders) was labeled "Obsidian
// vault" and shown the vault-only "Organize" button — because the old logic
// was `source_type !== "DocumentFolder" ? vault`. `source_type` is an
// externally-tagged union, so a watched folder is `{WatchedFolder: {...}}`,
// not the string "DocumentFolder".
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/svelte";
import LocalKnowledgeList from "./LocalKnowledgeList.svelte";
import type { LocalCorpusConfig, LocalCorpusSourceType } from "../../types";

function cfg(
  id: string,
  display_name: string,
  source_type: LocalCorpusSourceType,
): LocalCorpusConfig {
  return {
    id,
    display_name,
    root_path: `/tmp/${id}`,
    source_type,
  } as unknown as LocalCorpusConfig;
}

const noop = () => {};

describe("LocalKnowledgeList source labels", () => {
  it("labels a DocumentFolder 'Folder' with no Organize button", () => {
    render(LocalKnowledgeList, {
      props: { corpora: [cfg("f1", "Docs", "DocumentFolder")], onRemove: noop },
    });
    expect(screen.getByText("Folder")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /organize/i }),
    ).not.toBeInTheDocument();
  });

  it("labels an ObsidianVault 'Obsidian vault' and offers Organize", () => {
    render(LocalKnowledgeList, {
      props: {
        corpora: [
          cfg("v1", "My Vault", {
            ObsidianVault: { parse_frontmatter: true, follow_wiki_links: true },
          }),
        ],
        onRemove: noop,
      },
    });
    expect(screen.getByText("Obsidian vault")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /organize/i }),
    ).toBeInTheDocument();
  });

  it("labels a WatchedFolder 'Watched folder' with NO Organize button", () => {
    render(LocalKnowledgeList, {
      props: {
        corpora: [
          cfg("watched-abc", "Sovereign Test", {
            WatchedFolder: {} as unknown as LocalCorpusSourceType,
          } as unknown as LocalCorpusSourceType),
        ],
        onRemove: noop,
      },
    });
    // The regression: this used to read "Obsidian vault".
    expect(screen.getByText("Watched folder")).toBeInTheDocument();
    expect(screen.queryByText("Obsidian vault")).not.toBeInTheDocument();
    // And it must NOT get the vault-only Organizer affordance.
    expect(
      screen.queryByRole("button", { name: /organize/i }),
    ).not.toBeInTheDocument();
  });
});

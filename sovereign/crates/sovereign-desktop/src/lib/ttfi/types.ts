// SPDX-License-Identifier: AGPL-3.0-or-later
// Shared TTFI types — consumed by both the production-shipping
// recorder (src/lib/ttfi/recorder.ts) and the test-time scenario player
// (tests/e2e/fixtures/scenario-player.ts). Keeping them in one place
// means a recorded scenario from prod is wire-compatible with the
// harness's playScenario() with no conversion step.

export type NarrationPhase =
  | "routing_committed"
  | "retrieval_complete"
  | "primary_synthesis_start"
  | "gap_check_fired";

export type ScenarioEvent =
  | {
      atMs: number;
      kind: "doc-op";
      type: "Routing" | "Retrieving" | "AnalysingEntity" | "Synthesising";
      operation?: string;
      name?: string;
    }
  | { atMs: number; kind: "narration"; phase: NarrationPhase; text: string }
  | {
      atMs: number;
      kind: "interpretation";
      interpretation: string;
      alternatives: { label: string; intent_hint: string }[];
      confidence: number;
    }
  | {
      atMs: number;
      kind: "clarification";
      question: string;
      options: { label: string; follow_up: string; intent_hint: string }[];
    }
  | { atMs: number; kind: "chunk"; text: string }
  | { atMs: number; kind: "complete"; fullText: string; metadata?: unknown }
  | { atMs: number; kind: "error"; message: string };

export type ScenarioTerminal =
  | { kind: "send-btn-visible" }
  | { kind: "selector-visible"; selector: string };

export type ScenarioBudgets = {
  generic?: number;
  specific?: number;
  aux?: number;
  visible?: number;
  thinking?: number;
  content?: number;
  gap?: number;
  staleness?: number;
};

export type Scenario = {
  name: string;
  description: string;
  query: string;
  events: ScenarioEvent[];
  terminal: ScenarioTerminal;
  budgets?: ScenarioBudgets;
};

// Type declarations for the JS mock daemon fixture (co-located so an
// import of ./fixtures/mock-daemon.mjs resolves here).
export interface MockDaemon {
  state: {
    mode:
      | "happy"
      | "slow"
      | "error503"
      | "error400"
      | "noEdit"
      | "nextEditOnly"
      | "degraded"
      | "legacy";
    lastRequestBody: {
      prefix?: string;
      suffix?: string;
      stream?: boolean;
      debug?: boolean;
    } | null;
    aborted: boolean;
    lastEditPredictionBody: {
      history?: { before: string; after: string; left: string; right: string }[];
      text?: string;
      cursor?: number;
      debug?: boolean;
    } | null;
    editPrediction: {
      object?: string;
      engine?: string;
      episode_id?: string;
      edits?: { start: number; end: number; new_text: string }[];
      sovereign_debug?: Record<string, unknown>;
      /** Symbol-lane jump list, or `{ declined }` — a named state, never
       *  an absent key. Omitted entirely by a daemon predating the lane,
       *  which the client must report as `null` rather than `{}`. */
      navigation?: {
        symbol?: string;
        sites?: { path: string; line: number; col: number; preview: string }[];
        truncated?: boolean;
        dropped?: number;
        declined?: string;
      };
    };
    /** Outcome reports the extension fired at
     *  /v1/edit_predictions/outcome, in arrival order. */
    outcomes: { episode_id: string; outcome: string }[];
    /** Force a status on the outcome route: `404` stands in for a daemon
     *  older than the route, `500` for one that is up but broken. */
    outcomeStatus: number | null;
  };
  port: number;
  endpoint: string;
  close: () => Promise<void>;
}
export declare function startMockDaemon(): Promise<MockDaemon>;

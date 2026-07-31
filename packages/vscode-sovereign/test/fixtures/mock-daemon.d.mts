// Type declarations for the JS mock daemon fixture (co-located so an
// import of ./fixtures/mock-daemon.mjs resolves here).
export interface MockDaemon {
  state: {
    mode: "happy" | "slow" | "error503" | "noFim" | "error400";
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
    editPrediction: unknown;
  };
  port: number;
  endpoint: string;
  close: () => Promise<void>;
}
export declare function startMockDaemon(): Promise<MockDaemon>;

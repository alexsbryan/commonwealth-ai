// Type declarations for the JS mock daemon fixture (co-located so an
// import of ./fixtures/mock-daemon.mjs resolves here).
export interface MockDaemon {
  state: {
    mode: "happy" | "slow" | "error503" | "noFim";
    lastRequestBody: {
      prefix?: string;
      suffix?: string;
      stream?: boolean;
      debug?: boolean;
    } | null;
    aborted: boolean;
  };
  port: number;
  endpoint: string;
  close: () => Promise<void>;
}
export declare function startMockDaemon(): Promise<MockDaemon>;

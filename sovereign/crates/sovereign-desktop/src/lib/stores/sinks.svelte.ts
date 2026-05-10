import type { SinkStatusDto } from "../types";
import { getSinkStatus } from "../api";

let _status: SinkStatusDto = $state({ any_connected: false, sinks: [] });

export const sinkStore = {
  get status() {
    return _status;
  },
  get anyConnected() {
    return _status.any_connected;
  },

  async load() {
    try {
      _status = await getSinkStatus();
    } catch {
      // Sink status not critical — default to disconnected.
    }
  },
};

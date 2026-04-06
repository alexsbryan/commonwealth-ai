import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  StepDonePayload,
  ApprovalRequestPayload,
  UserInputRequestPayload,
  ErrorPayload,
  CorpusProgressPayload,
} from "./types";

export interface EventHandlers {
  onBackendReady?: () => void;
  onBackendError?: (error: string) => void;
  onSetupRequired?: () => void;
  onStepDone?: (payload: StepDonePayload) => void;
  onApprovalRequest?: (payload: ApprovalRequestPayload) => void;
  onUserInputRequest?: (payload: UserInputRequestPayload) => void;
  onError?: (payload: ErrorPayload) => void;
  onCorpusProgress?: (payload: CorpusProgressPayload) => void;
  onDeepLink?: (url: string) => void;
}

export async function initEventListeners(
  handlers: EventHandlers,
): Promise<UnlistenFn[]> {
  const unlisteners: UnlistenFn[] = [];

  if (handlers.onBackendReady) {
    unlisteners.push(
      await listen("backend-ready", () => handlers.onBackendReady!()),
    );
  }

  if (handlers.onBackendError) {
    unlisteners.push(
      await listen<ErrorPayload>("backend-error", (event) =>
        handlers.onBackendError!(event.payload.message),
      ),
    );
  }

  if (handlers.onSetupRequired) {
    unlisteners.push(
      await listen("setup-required", () => handlers.onSetupRequired!()),
    );
  }

  if (handlers.onStepDone) {
    unlisteners.push(
      await listen<StepDonePayload>("step-done", (event) =>
        handlers.onStepDone!(event.payload),
      ),
    );
  }

  if (handlers.onApprovalRequest) {
    unlisteners.push(
      await listen<ApprovalRequestPayload>("approval-request", (event) =>
        handlers.onApprovalRequest!(event.payload),
      ),
    );
  }

  if (handlers.onUserInputRequest) {
    unlisteners.push(
      await listen<UserInputRequestPayload>("user-input-request", (event) =>
        handlers.onUserInputRequest!(event.payload),
      ),
    );
  }

  if (handlers.onError) {
    unlisteners.push(
      await listen<ErrorPayload>("error", (event) =>
        handlers.onError!(event.payload),
      ),
    );
  }

  if (handlers.onCorpusProgress) {
    unlisteners.push(
      await listen<CorpusProgressPayload>("corpus-progress", (event) =>
        handlers.onCorpusProgress!(event.payload),
      ),
    );
  }

  if (handlers.onDeepLink) {
    unlisteners.push(
      await listen<string>("deep-link-received", (event) =>
        handlers.onDeepLink!(event.payload),
      ),
    );
  }

  return unlisteners;
}

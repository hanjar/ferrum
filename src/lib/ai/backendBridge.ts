import { invoke } from '@tauri-apps/api/core';
import type { BackendIntentTransport } from './remote-provider';
import type { BackendConnectionTransport } from './provider-setup';

// Thin Tauri bridges for the AI provider (FER-187/188/190). Rejections are passed through
// untouched: the Rust backend rejects with a data object `{ kind, attempts }` that
// `backendErrorKind` classifies. No credential ever crosses this boundary.

export const backendIntentTransport: BackendIntentTransport = async (input, signal) => {
  const onAbort = () => {
    void invoke('ai_cancel_plan', { requestId: input.requestId }).catch(() => undefined);
  };
  if (signal.aborted) onAbort();
  else signal.addEventListener('abort', onAbort, { once: true });
  try {
    return await invoke('ai_plan_intent', {
      config: input.config,
      request: {
        requestId: input.requestId,
        userText: input.userText,
        context: input.context,
        ...(input.capabilities ? { capabilities: input.capabilities } : {}),
      },
    });
  } finally {
    signal.removeEventListener('abort', onAbort);
  }
};

export const backendConnectionTransport: BackendConnectionTransport = (setup) =>
  invoke('ai_test_connection', { config: setup });

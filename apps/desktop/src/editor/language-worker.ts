import libraries from 'virtual:mado-es2020';
import {ScriptLanguageService} from './language-service.ts';
import type {CompletionRequest, CompletionResponse} from './completion-types.ts';

// Analysis has no host-command channel. Worker lifetime and deadlines belong to the client.
const analysis = new ScriptLanguageService(libraries);
const port = globalThis as unknown as {
  onmessage: ((event: MessageEvent<CompletionRequest>) => void) | null;
  postMessage: (response: CompletionResponse) => void;
};
port.onmessage = event => {
  const request = event.data;
  try {
    port.postMessage({kind: 'result', result: analysis.complete(request)});
  } catch {
    analysis.dispose();
    port.postMessage({kind: 'failed', key: request.key});
  }
};
port.postMessage({kind: 'ready'});

export interface CompletionKey {
  owner: string;
  path: string;
  revision: number;
  generation: number;
  request: number;
}

export interface CompletionContext {
  owner: string;
  path: string;
  revision: number;
  generation: number;
  source: string;
  schema: string | null;
  // Bytes of the other non-image drafts, including schema, in this package.
  otherBytes: number;
}

export interface CompletionCandidate {
  label: string;
  insertText: string;
  from: number;
  to: number;
  kind: string;
  detail?: string;
  detailOmitted?: boolean;
}

export interface CompletionAnalysis {
  candidates: CompletionCandidate[];
  capped: boolean;
  optionsAvailable: boolean;
}

export interface CompletionResult extends CompletionAnalysis { key: CompletionKey }
export type CompletionStatus = 'idle' | 'starting' | 'pending' | 'ready' | 'capped' | 'unavailable' | 'oversized';
export interface CompletionRequest {
  kind: 'complete';
  key: CompletionKey;
  position: number;
  // Omitted only when identical to the last dispatched document/schema.
  source?: string;
  schema?: string | null;
}
export type CompletionResponse = {kind: 'ready'}
  | {kind: 'result'; result: CompletionResult}
  | {kind: 'failed'; key: CompletionKey};

export const DECLARATION_BYTES = 2 * 1024 * 1024;
export const RESPONSE_BYTES = 256 * 1024;
export const CANDIDATE_LIMIT = 200;
export const DETAIL_BYTES = 4 * 1024;
export const STARTUP_MS = 5000;
export const REQUEST_MS = 2000;

import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES} from '../authoring.ts';
import {CANDIDATE_LIMIT, DETAIL_BYTES, REQUEST_MS, RESPONSE_BYTES, STARTUP_MS} from './completion-types.ts';
import type {CompletionContext, CompletionKey, CompletionRequest, CompletionResponse, CompletionResult, CompletionStatus} from './completion-types.ts';

interface WorkerPort {
  onmessage: ((event: MessageEvent<CompletionResponse>) => void) | null;
  onerror: ((event: ErrorEvent) => void) | null;
  onmessageerror: ((event: MessageEvent) => void) | null;
  postMessage(message: CompletionRequest): void;
  terminate(): void;
}
interface ClientEnvironment {
  createWorker: () => WorkerPort;
  setTimer: (callback: () => void, ms: number) => number;
  clearTimer: (timer: number) => void;
}
interface Pending {
  context: CompletionContext;
  key: CompletionKey;
  position: number;
  resolve: (result: CompletionResult | null) => void;
}
const encoder = new TextEncoder();
const bytes = (value: string) => encoder.encode(value).length;
const sameKey = (a: CompletionKey, b: CompletionKey) => a.owner === b.owner && a.path === b.path
  && a.revision === b.revision && a.generation === b.generation && a.request === b.request;
const sameContext = (a: CompletionContext, b: CompletionContext) => a.owner === b.owner && a.path === b.path
  && a.revision === b.revision && a.generation === b.generation && a.source === b.source
  && a.schema === b.schema && a.otherBytes === b.otherBytes;

// Owns analysis only. Drafts, host commands and publication never wait for this worker.
export class CompletionClient {
  private context: CompletionContext | null = null;
  private worker: WorkerPort | null = null;
  private ready = false;
  private failed = false;
  private disposed = false;
  private serial = 0;
  private accepted: CompletionKey | null = null;
  private active: Pending | null = null;
  private queued: Pending | null = null;
  private sent: CompletionContext | null = null;
  private timer: number | null = null;
  private status: CompletionStatus = 'idle';
  private readonly environment: ClientEnvironment;
  private readonly onStatus: (status: CompletionStatus) => void;

  constructor(onStatus: (status: CompletionStatus) => void, environment: Partial<ClientEnvironment> = {}) {
    this.onStatus = onStatus;
    this.environment = {
      createWorker: () => new Worker(new URL('./language-worker.ts', import.meta.url), {type: 'module'}),
      setTimer: (callback, ms) => setTimeout(callback, ms),
      clearTimer: timer => clearTimeout(timer),
      ...environment,
    };
  }

  setContext(context: CompletionContext | null): void {
    if (this.disposed || (context && this.context && sameContext(context, this.context))) return;
    const ownerChanged = context?.owner !== this.context?.owner;
    this.context = context ? {...context} : null;
    this.accepted = null;
    this.queued?.resolve(null);
    this.serial += 1;
    this.queued = null;
    // Settle obsolete consumers now, but retain the one active deadline until its worker responds.
    this.active?.resolve(null);
    if (context === null || ownerChanged) this.retire();
    this.publish(this.failed ? 'unavailable' : 'idle');
  }

  request(position: number, explicit: boolean): Promise<CompletionResult | null> {
    const context = this.context;
    if (this.disposed || context === null || (!explicit && this.failed)) return Promise.resolve(null);
    this.accepted = null;
    this.serial += 1;
    if (!Number.isInteger(position) || position < 0 || position > context.source.length) return Promise.resolve(null);
    if (context.source.length > AUTHORING_SOURCE_BYTES || bytes(context.source) > AUTHORING_SOURCE_BYTES
      || (context.schema !== null && (context.schema.length > AUTHORING_SOURCE_BYTES || bytes(context.schema) > AUTHORING_SOURCE_BYTES))
      || !Number.isSafeInteger(context.otherBytes) || context.otherBytes < 0
      || bytes(context.source) + context.otherBytes > AUTHORING_NON_IMAGE_BYTES) {
      this.retire();
      this.failed = true;
      this.publish('oversized');
      return Promise.resolve(null);
    }
    const key: CompletionKey = {owner: context.owner, path: context.path, revision: context.revision,
      generation: context.generation, request: this.serial};
    const result = new Promise<CompletionResult | null>(resolve => {
      this.queued?.resolve(null);
      this.queued = {context, key, position, resolve};
    });
    if (!this.worker) this.start();
    this.dispatch();
    return result;
  }

  accepts(key: CompletionKey): boolean {
    const context = this.context;
    return !this.disposed && context !== null && this.accepted !== null && sameKey(key, this.accepted)
      && key.owner === context.owner && key.path === context.path && key.revision === context.revision
      && key.generation === context.generation;
  }

  dispose(): void {
    this.disposed = true;
    this.context = null;
    this.retire();
  }

  private publish(status: CompletionStatus): void {
    if (this.status === status || this.disposed) return;
    this.status = status;
    this.onStatus(status);
  }

  private clearTimer(): void {
    if (this.timer !== null) this.environment.clearTimer(this.timer);
    this.timer = null;
  }

  private retire(): void {
    this.clearTimer();
    if (this.worker) {
      this.worker.onmessage = null;
      this.worker.onerror = null;
      this.worker.onmessageerror = null;
      this.worker.terminate();
    }
    this.worker = null;
    this.ready = false;
    this.sent = null;
    this.accepted = null;
    this.active?.resolve(null);
    this.queued?.resolve(null);
    this.active = null;
    this.queued = null;
  }

  private fail(): void {
    this.retire();
    this.failed = true;
    this.publish('unavailable');
  }

  private start(): void {
    this.failed = false;
    this.publish('starting');
    try {
      const worker = this.environment.createWorker();
      this.worker = worker;
      worker.onmessage = event => {
        if (this.worker !== worker || this.disposed) return;
        this.receive(event.data);
      };
      worker.onerror = event => { event.preventDefault(); if (this.worker === worker) this.fail(); };
      worker.onmessageerror = () => { if (this.worker === worker) this.fail(); };
      this.timer = this.environment.setTimer(() => this.fail(), STARTUP_MS);
    } catch {
      this.fail();
    }
  }

  private dispatch(): void {
    if (!this.worker || !this.ready || this.active || !this.queued) return;
    const pending = this.queued;
    this.queued = null;
    if (!this.context || !sameContext(pending.context, this.context)) { pending.resolve(null); return; }
    this.active = pending;
    const request: CompletionRequest = {kind: 'complete', key: pending.key, position: pending.position};
    if (!this.sent || this.sent.path !== pending.context.path || this.sent.source !== pending.context.source) request.source = pending.context.source;
    if (!this.sent || this.sent.schema !== pending.context.schema) request.schema = pending.context.schema;
    this.sent = pending.context;
    this.publish('pending');
    this.timer = this.environment.setTimer(() => this.fail(), REQUEST_MS);
    try { this.worker.postMessage(request); } catch { this.fail(); }
  }

  private receive(message: CompletionResponse): void {
    if (message?.kind === 'ready') {
      if (this.ready) { this.fail(); return; }
      this.clearTimer();
      this.ready = true;
      this.dispatch();
      return;
    }
    const pending = this.active;
    if (!this.ready || !pending || !message) { this.fail(); return; }
    const key = message.kind === 'result' ? message.result?.key : message.kind === 'failed' ? message.key : null;
    if (!key || !sameKey(key, pending.key)) { this.fail(); return; }
    this.clearTimer();
    if (message.kind !== 'result' || !this.validResult(message.result, pending.context.source)) { this.fail(); return; }
    this.active = null;
    if (this.context && sameContext(pending.context, this.context) && pending.key.request === this.serial) {
      this.accepted = pending.key;
      this.publish(message.result.capped ? 'capped' : 'ready');
      pending.resolve(message.result);
    } else {
      pending.resolve(null);
    }
    this.dispatch();
  }

  private validResult(result: CompletionResult, source: string): boolean {
    if (!Array.isArray(result.candidates) || result.candidates.length > CANDIDATE_LIMIT
      || typeof result.capped !== 'boolean' || typeof result.optionsAvailable !== 'boolean') return false;
    for (const candidate of result.candidates) {
      if (!candidate || typeof candidate.label !== 'string' || typeof candidate.insertText !== 'string'
        || typeof candidate.kind !== 'string' || !Number.isInteger(candidate.from) || !Number.isInteger(candidate.to)
        || candidate.from < 0 || candidate.to < candidate.from || candidate.to > source.length
        || (candidate.detail !== undefined && typeof candidate.detail !== 'string')
        || (candidate.documentation !== undefined && typeof candidate.documentation !== 'string')
        || (candidate.detailOmitted !== undefined && typeof candidate.detailOmitted !== 'boolean')
        || bytes(candidate.detail ?? '') + bytes(candidate.documentation ?? '') > DETAIL_BYTES) return false;
    }
    return bytes(JSON.stringify({kind: 'result', result})) <= RESPONSE_BYTES;
  }
}

import type {CollaborationResponse} from './authoring-controller.ts';

// The host emits this only to the main window; the payload names a ticket, never the request itself.
export const COLLABORATION_REQUEST = 'collaboration-request';
const TICKET = /^[!-~]{1,256}$/;
const UNAVAILABLE:CollaborationResponse = {owner: null, ok: false,
  error: {code: 'unavailable', message: 'The desktop authoring controller is unavailable', outcome: 'not_applied'}};
const MALFORMED:CollaborationResponse = {owner: null, ok: false,
  error: {code: 'invalid_request', message: 'The claimed request is malformed', outcome: 'not_applied'}};
// A failure after handling began may follow an applied edit, so its outcome is not claimed either way.
const INTERNAL:CollaborationResponse = {owner: null, ok: false,
  error: {code: 'internal', message: 'The desktop could not complete the request; describe again before deciding', outcome: 'unknown'}};

// The Tauri seam, injected so the lifecycle can be exercised without a WebView.
export interface CollaborationHost {
  invoke<T>(command:string, args?:Record<string,unknown>):Promise<T>;
  listen<T>(event:string, handler:(event:{payload:T}) => void):Promise<() => void>;
}

// Consumes main-window tickets: claim, handle against the current controller, then reply. Draft edits run
// synchronously; host operations may settle later. One bridge serves the window for its lifetime; `start` returns its
// teardown. Registration and invalidation run strictly in call order, so a quick stop/start (StrictMode, reconnection
// after the Application became unavailable) can never leave the host with a stale registration.
export class CollaborationBridge {
  readonly #host:CollaborationHost;
  #lifecycle:Promise<unknown> = Promise.resolve();

  constructor(host:CollaborationHost) {
    this.#host = host;
  }

  #sequence(step:() => Promise<unknown>) {
    this.#lifecycle = this.#lifecycle.then(step).catch(() => {
      // Connection failure leaves UI-only authoring fully usable; the host reports unavailability to clients.
    });
  }

  start(handle:(request:unknown) => CollaborationResponse|Promise<CollaborationResponse>):() => void {
    let alive = true;
    let unlisten:(() => void)|null = null;
    const host = this.#host;
    const stop = () => {
      if (!alive) return;
      alive = false;
      unlisten?.();
      unlisten = null;
      this.#sequence(() => host.invoke<null>('collaboration_unavailable'));
    };
    const reply = (ticket:string, response:CollaborationResponse) => {
      void host.invoke<null>('collaboration_reply', {ticket, response}).catch(() => {
        // A rejected reply leaves the client with the host's timeout/unknown outcome; nothing is replayed.
      });
    };
    const claim = async (ticket:string) => {
      let claimed:unknown;
      try {
        claimed = await host.invoke<unknown>('collaboration_claim', {ticket});
      } catch {
        return;
      }
      // Null: cancelled, expired or claimed elsewhere before dispatch, so nothing was admitted.
      if (claimed === null) return;
      if (typeof claimed !== 'object' || !('ticket' in claimed) || claimed.ticket !== ticket || !('request' in claimed)) {
        reply(ticket, MALFORMED);
        return;
      }
      // The registration may have ended while the claim was in flight; nothing is applied then.
      if (!alive) {
        reply(ticket, UNAVAILABLE);
        return;
      }
      let response:CollaborationResponse;
      try {
        response = await handle(claimed.request);
      } catch {
        response = INTERNAL;
      }
      reply(ticket, response);
    };
    const listening = host.listen<{ticket?:unknown}>(COLLABORATION_REQUEST, event => {
      const ticket = event.payload?.ticket;
      if (alive && typeof ticket === 'string' && TICKET.test(ticket)) void claim(ticket);
    });
    // Observed by the registration step; this only keeps an early rejection from being reported as unhandled.
    listening.catch(() => {});
    this.#sequence(async () => {
      let release:() => void;
      try {
        release = await listening;
      } catch (cause) {
        stop();
        throw cause;
      }
      if (!alive) {
        release();
        return;
      }
      unlisten = release;
      // Requests are admitted only after the listener exists, so no dispatched ticket goes unobserved.
      try {
        const ready = await host.invoke<{instance?:unknown; protocol?:unknown}|null>('collaboration_ready');
        if (ready?.protocol !== 1 || typeof ready.instance !== 'string') stop();
      } catch (cause) {
        stop();
        throw cause;
      }
    });
    return stop;
  }
}

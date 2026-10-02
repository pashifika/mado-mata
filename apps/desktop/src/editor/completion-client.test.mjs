import {test} from 'node:test';
import assert from 'node:assert/strict';
import {CompletionClient} from './completion-client.ts';
import {REQUEST_MS, STARTUP_MS, CANDIDATE_LIMIT, RESPONSE_BYTES} from './completion-types.ts';
import {AUTHORING_SOURCE_BYTES} from '../authoring.ts';

function harness() {
  const workers = [], statuses = [], timers = new Map();
  let now = 0, timerId = 0;
  const client = new CompletionClient(status => statuses.push(status), {
    createWorker() {
      const worker = {onmessage: null, onerror: null, onmessageerror: null, terminated: false, requests: [],
        postMessage(message) { this.requests.push(message); },
        terminate() { this.terminated = true; },
        send(message) { this.onmessage?.({data: message}); },
        ready() { this.send({kind: 'ready'}); },
        result(index = this.requests.length - 1, extra = {}) {
          this.send({kind: 'result', result: {key: this.requests[index].key, candidates: [], capped: false, optionsAvailable: true, ...extra}});
        },
      };
      workers.push(worker);
      return worker;
    },
    setTimer(callback, ms) { const id = ++timerId; timers.set(id, {at: now + ms, callback}); return id; },
    clearTimer(id) { timers.delete(id); },
  });
  return {client, workers, statuses, advance(ms) {
    now += ms;
    for (const [id, timer] of [...timers]) if (timer.at <= now && timers.delete(id)) timer.callback();
  }};
}
const context = {owner: 'lease-1', path: 'main.ts', revision: 1, generation: 1, source: 'host.', schema: null, otherBytes: 0};

test('coalesces superseded requests without renewing the active deadline', async () => {
  const h = harness();
  h.client.setContext(context);
  const first = h.client.request(5, true);
  const worker = h.workers[0];
  worker.ready();
  h.advance(REQUEST_MS - 1);
  const second = h.client.request(4, true);
  const latest = h.client.request(3, true);
  assert.equal(await second, null);
  assert.equal(worker.requests.length, 1);
  h.advance(1);
  assert.equal(await first, null);
  assert.equal(await latest, null);
  assert.equal(worker.terminated, true);
  assert.equal(h.statuses.at(-1), 'unavailable');
  assert.equal(await h.client.request(5, false), null);
  assert.equal(h.workers.length, 1);
  const retry = h.client.request(5, true);
  h.workers[1].ready();
  h.workers[1].result();
  assert.equal(h.client.accepts((await retry).key), true);
  h.client.dispose();
});

test('only the latest eligible request publishes, with changed document payloads', async () => {
  const h = harness();
  h.client.setContext(context);
  const first = h.client.request(5, true);
  const worker = h.workers[0];
  worker.ready();
  const older = h.client.request(4, true);
  const latest = h.client.request(3, true);
  assert.equal(await older, null);
  worker.result(0);
  assert.equal(await first, null);
  assert.equal(worker.requests.length, 2);
  assert.equal(worker.requests[1].position, 3);
  assert.equal(Object.hasOwn(worker.requests[1], 'source'), false);
  assert.equal(Object.hasOwn(worker.requests[1], 'schema'), false);
  worker.result(1, {capped: true});
  const result = await latest;
  assert.equal(h.client.accepts(result.key), true);
  assert.equal(h.statuses.at(-1), 'capped');
  const changed = {...context, revision: 2, source: 'host.call', schema: '{}', generation: 2};
  h.client.setContext(changed);
  assert.equal(h.client.accepts(result.key), false);
  const next = h.client.request(9, true);
  assert.equal(worker.requests[2].source, changed.source);
  assert.equal(worker.requests[2].schema, '{}');
  worker.result(2);
  assert.equal(h.client.accepts((await next).key), true);
  h.client.dispose();
});

test('context changes and successor owners reject obsolete publication and acceptance', async () => {
  const h = harness();
  h.client.setContext(context);
  const obsolete = h.client.request(5, true);
  const originalWorker = h.workers[0];
  originalWorker.ready();
  h.client.setContext({...context, generation: 2});
  assert.equal(await obsolete, null);
  // Even a return to identical context cannot revive an invalidated request.
  h.client.setContext(context);
  originalWorker.result();
  assert.equal(h.client.accepts(originalWorker.requests[0].key), false);
  const accepted = h.client.request(5, true);
  originalWorker.result();
  const published = await accepted;
  h.client.setContext({...context, owner: 'lease-2'});
  assert.equal(h.client.accepts(published.key), false);
  assert.equal(originalWorker.terminated, true);
  const successor = h.client.request(5, true);
  h.workers[1].ready();
  h.workers[1].result();
  assert.equal((await successor).key.owner, 'lease-2');
  h.client.setContext(null);
  assert.equal(h.client.accepts(published.key), false);
  h.client.dispose();
});

test('startup and worker failures settle requests and require explicit recovery', async () => {
  const h = harness();
  h.client.setContext(context);
  const request = h.client.request(5, true);
  h.advance(STARTUP_MS);
  assert.equal(await request, null);
  assert.equal(h.workers[0].terminated, true);
  h.client.setContext({...context, revision: 2});
  assert.equal(await h.client.request(5, false), null);
  assert.equal(h.workers.length, 1);
  const retry = h.client.request(5, true);
  h.workers[1].ready();
  h.workers[1].onerror({preventDefault() {}});
  assert.equal(await retry, null);
  assert.equal(h.workers[1].terminated, true);
  const final = h.client.request(5, true);
  h.workers[2].ready();
  h.workers[2].result();
  assert.equal(h.client.accepts((await final).key), true);
  h.client.dispose();
});

test('disposal settles active and queued work and ignores a retired worker callback', async () => {
  const h = harness();
  h.client.setContext(context);
  const active = h.client.request(5, true);
  const worker = h.workers[0];
  worker.ready();
  const deliver = worker.onmessage;
  const queued = h.client.request(4, true);
  h.client.dispose();
  assert.equal(await active, null);
  assert.equal(await queued, null);
  deliver({data: {kind: 'result', result: {key: worker.requests[0].key, candidates: [], optionsAvailable: true, capped: false}}});
  assert.equal(h.client.accepts(worker.requests[0].key), false);
  assert.equal(await h.client.request(5, true), null);
  assert.equal(h.workers.length, 1);
  assert.equal(worker.terminated, true);
});

test('oversized analysis refuses before worker startup and later smaller explicit work remains eligible', async () => {
  const h = harness();
  h.client.setContext({...context, source: 'あ'.repeat(Math.floor(AUTHORING_SOURCE_BYTES / 3) + 1)});
  assert.equal(await h.client.request(0, true), null);
  assert.equal(h.workers.length, 0);
  assert.equal(h.statuses.at(-1), 'oversized');
  h.client.setContext(context);
  assert.equal(await h.client.request(5, false), null);
  const smaller = h.client.request(5, true);
  h.workers[0].ready();
  h.workers[0].result();
  assert.equal(h.client.accepts((await smaller).key), true);
  h.client.dispose();
});

test('out-of-bounds worker results are unavailable, not acceptable truncated edits', async () => {
  const h = harness();
  h.client.setContext(context);
  const first = h.client.request(5, true);
  h.workers[0].ready();
  h.workers[0].result(0, {candidates: Array.from({length: CANDIDATE_LIMIT + 1}, () => ({label: 'call', insertText: 'call', from: 5, to: 5, kind: 'method'}))});
  assert.equal(await first, null);
  assert.equal(h.workers[0].terminated, true);
  const second = h.client.request(5, true);
  h.workers[1].ready();
  h.workers[1].result(0, {candidates: [{label: 'x'.repeat(RESPONSE_BYTES), insertText: 'call', from: 5, to: 5, kind: 'method'}]});
  assert.equal(await second, null);
  assert.equal(h.client.accepts(h.workers[1].requests[0].key), false);
  const third = h.client.request(5, true);
  h.workers[2].ready();
  h.workers[2].result(0, {candidates: [{label: 'call', insertText: 'call', from: 4, to: 6, kind: 'method'}]});
  assert.equal(await third, null);
  assert.equal(h.statuses.at(-1), 'unavailable');
  h.client.dispose();
});

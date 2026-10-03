import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EditorState, Transaction} from '@codemirror/state';
import {javascript} from '@codemirror/lang-javascript';
import {trustedLibraries} from '../../build/trusted-libraries.mjs';
import {CompletionSession} from './completion-session.ts';
import {CompletionClient} from './completion-client.ts';
import {ScriptLanguageService} from './language-service.ts';
import {SourcePositions} from './source-positions.ts';
import {CANDIDATE_LIMIT, REQUEST_MS} from './completion-types.ts';

const libraries = trustedLibraries();
const typing = {type: 'insertText', data: null, composing: false};
const backward = {type: 'deleteContentBackward', data: null, composing: false};

// Only the worker transport and clock are controlled: transactions, source mapping,
// session admission, client fences and the TypeScript producer are production code.
function harness(t, marked = 'host.call("¦")', preferences = {automatic: true, delay_ms: 100}) {
  const position = marked.indexOf('¦');
  assert.notEqual(position, -1);
  let mapping = new SourcePositions(marked.replace('¦', ''));
  let state = EditorState.create({doc: mapping.document, selection: {anchor: mapping.toEditor(position)}, extensions: [javascript({typescript: true})]});
  let context = {owner: 'lease-1', path: 'main.ts', revision: 1, generation: 1, source: mapping.source, schema: null, otherBytes: 0};
  let focused = true, composing = false, readOnly = false, visible = null;
  const workers = [], timers = new Map(), publications = [], statuses = [];
  let now = 0, timerId = 0;
  const clock = {
    setTimer(callback, ms) { const id = ++timerId; timers.set(id, {at: now + ms, callback}); return id; },
    clearTimer(id) { timers.delete(id); },
  };
  const client = new CompletionClient(status => statuses.push(status), {...clock,
    createWorker() {
      const service = new ScriptLanguageService(libraries);
      const worker = {onmessage: null, onerror: null, onmessageerror: null, requests: [], terminated: false,
        postMessage(message) { this.requests.push(message); },
        terminate() { this.terminated = true; service.dispose(); },
        ready() { this.onmessage?.({data: {kind: 'ready'}}); },
        respond(index = this.requests.length - 1) {
          const result = service.complete(this.requests[index]);
          this.onmessage?.({data: {kind: 'result', result}});
          return result;
        },
      };
      workers.push(worker);
      return worker;
    },
  });
  client.setContext(context);
  const session = new CompletionSession({...clock,
    read: () => ({state, mapping, context, focused, composing, readOnly, preferences}),
    request: (position, explicit) => client.request(position, explicit),
    accepts: (result, source) => source === mapping.source && client.accepts(result.key),
    close: () => { visible = null; },
    publish: () => { visible = session.current(); publications.push(visible); },
  });
  t.after(() => { session.cancel(); client.dispose(); });
  const h = {session, workers, publications, statuses,
    get state() { return state; }, get source() { return mapping.source; }, get context() { return context; },
    get visible() { return visible; },
    advance(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) if (timer.at <= now && timers.delete(id)) timer.callback();
    },
    commit(extra = {}) {
      context = {...context, source: mapping.source, revision: context.revision + 1, generation: context.generation + 1, ...extra};
      client.setContext(context);
      session.synchronize();
    },
    change(spec, input = null, commit = true) {
      const transaction = state.update(spec);
      const changes = [];
      transaction.changes.iterChanges((from, to, _newFrom, _newTo, inserted) => changes.push({from, to, insert: inserted.toString()}));
      mapping = mapping.apply(changes);
      state = transaction.state;
      session.update([transaction], [input]);
      if (transaction.docChanged && commit) h.commit();
      return transaction;
    },
    type(text, commit = true) {
      const from = state.selection.main.head;
      return h.change({changes: {from, insert: text}, selection: {anchor: from + text.length},
        annotations: Transaction.userEvent.of('input.type')}, {...typing, data: text}, commit);
    },
    backspace() {
      const to = state.selection.main.head;
      return h.change({changes: {from: to - 1, to}, selection: {anchor: to - 1},
        annotations: Transaction.userEvent.of('delete.backward')}, backward);
    },
    preferences(value) { preferences = value; session.synchronize(); },
    focus(value) { focused = value; session.synchronize(); },
    readOnly(value) { readOnly = value; session.synchronize(); },
    composition(value) { composing = value; session.synchronize(); },
    accept(publication, label) {
      if (!session.accepts(publication)) return false;
      const candidate = publication.result.candidates.find(candidate => candidate.label === label);
      assert.ok(candidate);
      const from = mapping.toEditor(candidate.from), to = mapping.toEditor(candidate.to);
      session.cancel();
      h.change({changes: {from, to, insert: candidate.insertText}, selection: {anchor: from + candidate.insertText.length},
        annotations: Transaction.userEvent.of('input.complete')}, {type: 'insertReplacementText', data: null, composing: false});
      return true;
    },
  };
  return h;
}

for (const delay of [0, 100, 1000]) {
  test(`automatic opening rechecks authoritative source after the saved ${delay} ms delay`, async t => {
    const h = harness(t, 'host.call("¦")', {automatic: true, delay_ms: delay});
    h.type('r', false);
    h.advance(delay);
    assert.equal(h.workers.length, 0, 'a timer cannot query the pre-edit owner context');
    h.commit();
    assert.equal(h.workers.length, 1);
    h.workers[0].ready();
    h.workers[0].respond();
    await Promise.resolve();
    assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label).sort(), ['recognize', 'release']);
  });
}

test('rapid input supersedes opening delay and then coalesces live requests without delaying refresh', async t => {
  const h = harness(t);
  h.type('r');
  h.advance(99);
  h.type('e');
  h.advance(99);
  assert.equal(h.workers.length, 0);
  h.advance(1);
  const worker = h.workers[0];
  worker.ready();
  h.type('l');
  h.type('e');
  assert.equal(worker.requests.length, 1);
  worker.respond(0);
  assert.equal(worker.requests.length, 2, 'the client keeps only the latest current-source request');
  assert.equal(worker.requests[1].source, 'host.call("rele")');
  worker.respond(1);
  await Promise.resolve();
  assert.equal(h.publications.length, 1);
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label), ['release']);
});

for (const live of [false, true]) {
  test(`a member trigger replaces a ${live ? 'live' : 'scheduled'} identifier session with a delayed new context`, async t => {
    const h = harness(t, '¦', {automatic: true, delay_ms: 1000});
    h.type('host');
    if (live) {
      h.advance(1000);
      h.workers[0].ready();
      h.workers[0].respond();
      await Promise.resolve();
      assert.ok(h.visible.result.candidates.some(candidate => candidate.label === 'host'));
    }
    h.type('.');
    assert.equal(h.visible, null);
    h.advance(999);
    if (live) assert.equal(h.workers[0].requests.length, 1);
    else assert.equal(h.workers.length, 0);
    h.advance(1);
    const worker = h.workers[0];
    if (!live) worker.ready();
    worker.respond();
    await Promise.resolve();
    assert.ok(h.visible.result.candidates.some(candidate => candidate.label === 'call'));
    assert.ok(h.visible.result.candidates.some(candidate => candidate.label === 'options'));
    assert.equal(h.accept(h.visible, 'call'), true);
    assert.equal(h.source, 'host.call');
  });
}

test('manual-only continuation invalidates old actions and Backspace broadens from rel to re', async t => {
  const h = harness(t, 'host.call("¦")', {automatic: false, delay_ms: 1000});
  h.type('r');
  h.advance(1000);
  assert.equal(h.workers.length, 0);
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  worker.respond();
  await Promise.resolve();
  const original = h.visible;
  const order = original.result.candidates.map(candidate => candidate.label);
  h.type('el');
  assert.equal(h.visible, null);
  assert.equal(h.session.accepts(original), false);
  assert.equal(h.accept(original, 'recognize'), false);
  assert.equal(h.source, 'host.call("rel")');
  assert.equal(worker.requests.length, 2, 'manual refresh bypasses the 1000 ms opening delay');
  worker.respond();
  await Promise.resolve();
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label), ['release']);
  const narrowed = h.visible;
  h.backspace();
  assert.equal(h.visible, null);
  assert.equal(h.session.accepts(narrowed), false);
  worker.respond();
  await Promise.resolve();
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label), order);
  assert.equal(h.accept(h.visible, 'recognize'), true);
  assert.equal(h.source, 'host.call("recognize")');
  h.advance(1000);
  assert.equal(h.visible, null, 'acceptance does not create a new session');
  assert.equal(worker.requests.length, 3);
});

test('Backspace recovers candidates outside a previously capped provider subset in stable order', async t => {
  const fields = ['recognize: 1', ...Array.from({length: CANDIDATE_LIMIT + 10}, (_, index) => `release${String(index).padStart(3, '0')}: 1`)];
  const h = harness(t, `const local = {${fields.join(', ')}}; local.rel¦; export {};`);
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  worker.respond();
  await Promise.resolve();
  const narrow = h.visible.result;
  assert.equal(narrow.capped, true);
  assert.equal(narrow.candidates.length, CANDIDATE_LIMIT);
  assert.equal(narrow.candidates.some(candidate => candidate.label === 'recognize'), false);
  h.backspace();
  worker.respond();
  await Promise.resolve();
  const broad = h.visible.result;
  assert.equal(broad.capped, true);
  assert.ok(broad.candidates.some(candidate => candidate.label === 'recognize'));
  assert.ok(broad.candidates.length <= CANDIDATE_LIMIT);
  const broadNames = new Set(broad.candidates.map(candidate => candidate.label));
  const narrowNames = new Set(narrow.candidates.map(candidate => candidate.label));
  assert.deepEqual(broad.candidates.filter(candidate => narrowNames.has(candidate.label)).map(candidate => candidate.label),
    narrow.candidates.filter(candidate => broadNames.has(candidate.label)).map(candidate => candidate.label));
});

const dismissals = [
  {scenario: 'Escape', dismiss: h => h.session.cancel()},
  {scenario: 'blur', dismiss: h => h.focus(false)},
  {scenario: 'caret movement', dismiss: h => h.change({selection: {anchor: 0}})},
  {scenario: 'nonempty selection', dismiss: h => h.change({selection: {anchor: h.state.selection.main.head, head: 0}})},
  {scenario: 'newline', dismiss: h => h.type('\n')},
  {scenario: 'closing literal context', dismiss: h => h.type('"')},
  {scenario: 'composition start', dismiss: h => h.composition(true)},
  {scenario: 'read-only transition', dismiss: h => h.readOnly(true)},
  {scenario: 'schema generation', dismiss: h => h.commit({schema: '{}'})},
  {scenario: 'equal-text revision', dismiss: h => h.commit()},
  {scenario: 'file switch', dismiss: h => h.commit({path: 'other.ts'})},
  {scenario: 'owner switch', dismiss: h => h.commit({owner: 'lease-2'})},
];
for (const {scenario, dismiss} of dismissals) {
  test(`${scenario} cancels delayed opening without querying`, t => {
    const h = harness(t);
    h.type('r');
    dismiss(h);
    h.advance(1000);
    assert.equal(h.workers.length, 0);
    assert.equal(h.visible, null);
  });
  test(`${scenario} prevents a delayed worker response from reviving dismissed work`, async t => {
    const h = harness(t, 'host.call("r¦")');
    h.session.explicit();
    const worker = h.workers[0];
    worker.ready();
    const deliver = worker.onmessage;
    // Compute before a possible owner retirement disposes the producer, but defer delivery.
    const service = new ScriptLanguageService(libraries);
    const result = service.complete(worker.requests[0]);
    service.dispose();
    dismiss(h);
    deliver({data: {kind: 'result', result}});
    await Promise.resolve();
    assert.equal(h.visible, null);
    assert.equal(h.publications.length, 0);
  });
}

test('an equal-text successor cannot reuse a dismissed candidate or request identity', async t => {
  const h = harness(t, 'host.call("re¦")');
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  worker.respond();
  await Promise.resolve();
  const original = h.visible;
  h.session.cancel();
  h.session.explicit();
  assert.equal(h.accept(original, 'release'), false);
  worker.respond();
  await Promise.resolve();
  assert.equal(h.source, 'host.call("re")');
  assert.notEqual(h.visible.result.key.request, original.result.key.request);
  assert.equal(h.session.accepts(original), false);
  assert.equal(h.accept(h.visible, 'release'), true);
  assert.equal(h.source, 'host.call("release")');
});

for (const automatic of [true, false]) {
  test(`no match closes a ${automatic ? 'automatic' : 'manual'} session; deletion alone cannot reopen it`, async t => {
    const h = harness(t, 'host.call("¦")', {automatic, delay_ms: 100});
    h.type('r');
    if (automatic) h.advance(100); else h.session.explicit();
    const worker = h.workers[0];
    worker.ready();
    worker.respond();
    await Promise.resolve();
    h.type('z');
    worker.respond();
    await Promise.resolve();
    assert.equal(h.visible, null);
    h.backspace();
    h.advance(1000);
    assert.equal(worker.requests.length, 2);
    h.session.explicit();
    worker.respond();
    await Promise.resolve();
    assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label).sort(), ['recognize', 'release']);
  });
}

test('composition, an intervening selection transaction and its commit stay closed until later direct typing', async t => {
  const h = harness(t, 'host.call("¦")');
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  h.composition(true);
  const from = h.state.selection.main.head;
  h.change({changes: {from, insert: 'r'}, selection: {anchor: from + 1},
    annotations: Transaction.userEvent.of('input.type.compose')}, {type: 'insertCompositionText', data: 'r', composing: true});
  h.change({selection: {anchor: from + 1}});
  h.composition(false);
  h.change({changes: {from, to: from + 1, insert: 're'}, selection: {anchor: from + 2},
    annotations: Transaction.userEvent.of('input.type.compose')}, {type: 'insertText', data: 're', composing: false});
  worker.respond();
  await Promise.resolve();
  h.advance(1000);
  assert.equal(h.visible, null);
  assert.equal(worker.requests.length, 1);
  assert.equal(h.source, 'host.call("re")');
  h.type('l');
  h.advance(100);
  worker.respond();
  await Promise.resolve();
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label), ['release']);
});

test('a failed manual request cannot transfer retry permission to its refresh', async t => {
  const h = harness(t, 'host.call("r¦")', {automatic: false, delay_ms: 1000});
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  h.advance(REQUEST_MS);
  assert.equal(worker.terminated, true);
  // Edit before the failed request's promise settles: the session still has manual intent.
  h.type('e');
  await Promise.resolve();
  h.type('l');
  h.advance(1000);
  assert.equal(h.workers.length, 1);
  assert.equal(h.visible, null);
  assert.equal(h.statuses.at(-1), 'unavailable');
  h.session.explicit();
  assert.equal(h.workers.length, 2);
  h.workers[1].ready();
  h.workers[1].respond();
  await Promise.resolve();
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label), ['release']);
});

test('saved preference values cancel in place while equal new objects preserve current publication', async t => {
  const h = harness(t, 'host.call("r¦")');
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  worker.respond();
  await Promise.resolve();
  const original = h.visible, state = h.state;
  h.preferences({automatic: true, delay_ms: 100});
  assert.equal(h.visible, original);
  assert.equal(h.session.accepts(original), true);
  h.preferences({automatic: false, delay_ms: 0});
  assert.equal(h.visible, null);
  assert.equal(h.session.accepts(original), false);
  assert.equal(h.state, state, 'the session owner never changes document, selection or history state');
  h.type('e');
  h.advance(1000);
  assert.equal(worker.requests.length, 1);
  h.preferences({automatic: true, delay_ms: 1000});
  h.focus(false);
  h.focus(true);
  h.advance(1000);
  assert.equal(worker.requests.length, 1, 'enabling or restoring focus is not a typing event');
});

for (const row of [
  {scenario: 'paste', event: 'input.paste', input: {type: 'insertFromPaste', data: 'r', composing: false}},
  {scenario: 'Undo', event: 'undo', input: {type: 'historyUndo', data: null, composing: false}},
  {scenario: 'Redo', event: 'redo', input: {type: 'historyRedo', data: null, composing: false}},
  {scenario: 'acceptance', event: 'input.complete', input: {type: 'insertReplacementText', data: null, composing: false}},
]) {
  test(`${row.scenario} never opens an automatic session`, t => {
    const h = harness(t);
    const from = h.state.selection.main.head;
    h.change({changes: {from, insert: 'r'}, selection: {anchor: from + 1}, annotations: Transaction.userEvent.of(row.event)}, row.input);
    h.advance(1000);
    assert.equal(h.workers.length, 0);
  });
}

test('a fresh explicit ticket fences a delayed response even with identical source and caret', async t => {
  const h = harness(t, 'host.call("r¦")');
  h.session.explicit();
  const worker = h.workers[0];
  worker.ready();
  const original = worker.requests[0].key;
  h.session.cancel();
  h.session.explicit();
  worker.respond(0);
  await Promise.resolve();
  assert.equal(h.visible, null);
  assert.equal(worker.requests.length, 2);
  worker.respond(1);
  await Promise.resolve();
  assert.notEqual(h.visible.result.key.request, original.request);
  assert.equal(h.publications.length, 1);
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label).sort(), ['recognize', 'release']);
});

test('changing a saved delay cancels scheduled work; only later typing uses the new delay', async t => {
  const h = harness(t);
  h.type('r');
  const state = h.state;
  h.preferences({automatic: true, delay_ms: 1000});
  assert.equal(h.state, state);
  h.advance(1000);
  assert.equal(h.workers.length, 0);
  h.type('e');
  h.advance(999);
  assert.equal(h.workers.length, 0);
  h.advance(1);
  h.workers[0].ready();
  h.workers[0].respond();
  await Promise.resolve();
  assert.deepEqual(h.visible.result.candidates.map(candidate => candidate.label).sort(), ['recognize', 'release']);
});

for (const suffix of ['"]; export {};', '']) {
  test(`escaped literal typing preserves a manual session with ${suffix ? 'a later closing quote' : 'an unterminated EOF'}`, async t => {
    const prefix = 'const local = {\'read"less\': 1, \'read"more\': 2}; local["';
    const h = harness(t, `${prefix}rea¦${suffix}`, {automatic: false, delay_ms: 1000});
    h.session.explicit();
    const worker = h.workers[0];
    worker.ready();
    worker.respond();
    await Promise.resolve();
    const original = h.visible;
    assert.ok(original);
    h.type('d\\"');
    assert.equal(h.visible, null);
    assert.equal(worker.requests.length, 2);
    worker.respond();
    await Promise.resolve();
    assert.equal(h.session.accepts(original), false);
    const candidate = h.visible.result.candidates.find(candidate => candidate.insertText === 'read\\"less');
    assert.ok(candidate);
    assert.equal(h.accept(h.visible, candidate.label), true);
    assert.equal(h.source, `${prefix}read\\"less${suffix}`);
  });
}

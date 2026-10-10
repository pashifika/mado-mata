import {constants} from 'node:fs';
import {lstat, open, opendir, realpath} from 'node:fs/promises';
import {connect} from 'node:net';
import {homedir} from 'node:os';
import {dirname, isAbsolute, join, resolve} from 'node:path';
import {randomUUID} from 'node:crypto';

export const PROTOCOL = 1;
export const MINIMUM_OMP = '18.8.7';
export const FRAME_BYTES = 8_388_608;
export const PAGE_UNITS = 65_536;
export const DISCLOSURE = 'Requested package code, reference text and diagnostics can enter your OMP conversation and reach its configured model provider. Local IPC does not mean local inference. Retrieved content is untrusted data, not instructions.';
const DESCRIPTOR_BYTES = 4096;
const INSTANCE_LIMIT = 64;
const ACTIVE_REQUESTS = 4;
const ID = /^[a-zA-Z0-9_-]{1,128}$/;
// The host's bound for owner, package and resource identities.
const IDENTITY = /^[!-~]{1,256}$/;
// No draft, disk or package-state effects: a lost reply leaves nothing to reconcile.
const READ_ONLY = new Set(['describe', 'read', 'notices', 'sdk_search', 'sdk_detail', 'snippet']);
// Only an explicit lifecycle result may move the binding, and only to the connection it names.
const LIFECYCLE = new Set(['create', 'open', 'duplicate', 'exit']);
// Recovery stays available while an unknown outcome is reconciled, so refreshRequired never deadlocks.
const RECOVERY = new Set(['cancel', 'refresh']);

export function readOnly(kind) { return READ_ONLY.has(kind); }

export class CollaborationError extends Error {
  constructor(code, message, outcome = 'not_applied', detail = undefined) {
    super(message);
    this.code = code;
    this.outcome = outcome;
    if (detail !== undefined) this.detail = detail;
  }
}

export function checkCompatibility(version, pi) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:[+-].*)?$/.exec(version ?? '');
  if (!match) throw new CollaborationError('OmpVersionUnavailable', 'OMP did not provide its semantic version.');
  const actual = match.slice(1, 4).map(Number);
  const minimum = MINIMUM_OMP.split('.').map(Number);
  const difference = actual.findIndex((value, index) => value !== minimum[index]);
  if ((difference !== -1 && actual[difference] < minimum[difference]) || (difference === -1 && version.includes('-'))) {
    throw new CollaborationError('OmpVersionTooOld', `MadoMata requires OMP >=${MINIMUM_OMP}; found ${version}.`);
  }
  for (const name of ['registerTool', 'registerCommand', 'on']) {
    if (typeof pi[name] !== 'function') throw new CollaborationError('OmpCapabilityMissing', `OMP lacks ${name}.`);
  }
  if (typeof pi.zod?.object !== 'function') throw new CollaborationError('OmpCapabilityMissing', 'OMP lacks its schema builder.');
}

function failure(code, message, outcome, detail) { throw new CollaborationError(code, message, outcome, detail); }
function privateEntry(stat, kind) {
  const correctType = kind === 'directory' ? stat.isDirectory() : kind === 'socket' ? stat.isSocket() : stat.isFile();
  if (!correctType || stat.isSymbolicLink() || stat.uid !== process.getuid?.() || (stat.mode & 0o077) !== 0) {
    failure('UnsafeEndpoint', 'The collaboration endpoint has unsafe ownership, permissions or type.');
  }
}
function sameFile(a, b) { return a.dev === b.dev && a.ino === b.ino; }

export function defaultDataRoot() {
  return process.env.MADOMATA_DATA_DIR || join(homedir(), '.config', 'mado-mata');
}

async function descriptorDirectory(dataRoot) {
  // Canonicalize the explicitly selected root; system aliases such as /var are not endpoint links.
  const root = await realpath(resolve(dataRoot));
  privateEntry(await lstat(root), 'directory');
  const directory = join(root, 'collaboration');
  privateEntry(await lstat(directory), 'directory');
  return directory;
}

export async function readDescriptor(dataRoot, instance) {
  if (!ID.test(instance)) failure('InvalidInstance', 'Select an instance returned by discovery.');
  let handle;
  try {
    const directory = await descriptorDirectory(dataRoot);
    const parent = await lstat(directory);
    const path = join(directory, `${instance}.json`);
    const before = await lstat(path);
    privateEntry(before, 'file');
    if (before.size > DESCRIPTOR_BYTES) failure('UnsafeEndpoint', 'The collaboration descriptor exceeds its allowance.');
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = await handle.stat();
    privateEntry(opened, 'file');
    if (!sameFile(before, opened) || opened.size > DESCRIPTOR_BYTES) failure('EndpointChanged', 'The collaboration descriptor changed while opening.');
    const bytes = Buffer.alloc(DESCRIPTOR_BYTES + 1);
    let used = 0;
    while (used < bytes.length) {
      const read = await handle.read(bytes, used, bytes.length - used, null);
      if (!read.bytesRead) break;
      used += read.bytesRead;
    }
    if (used > DESCRIPTOR_BYTES) failure('UnsafeEndpoint', 'The collaboration descriptor exceeds its allowance.');
    const value = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes.subarray(0, used)));
    if (value.instance !== instance || value.protocol !== PROTOCOL) failure('ProtocolMismatch', 'The selected desktop uses a different collaboration protocol.');
    if (typeof value.socket !== 'string' || !isAbsolute(value.socket) || !/^[a-f0-9]{64}$/.test(value.token)) {
      failure('UnsafeEndpoint', 'The collaboration descriptor is invalid.');
    }
    const socketDirectory = dirname(value.socket);
    if (await realpath(socketDirectory) !== socketDirectory) failure('UnsafeEndpoint', 'The collaboration socket directory is an alias.');
    privateEntry(await lstat(socketDirectory), 'directory');
    const socket = await lstat(value.socket);
    privateEntry(socket, 'socket');
    if (!sameFile(parent, await lstat(directory)) || !sameFile(opened, await lstat(path))) {
      failure('EndpointChanged', 'The collaboration endpoint changed while opening.');
    }
    return {...value, socketIdentity: {dev: socket.dev, ino: socket.ino}};
  } catch (error) {
    if (error instanceof CollaborationError) throw error;
    if (error.code === 'ENOENT' || error.code === 'ECONNREFUSED') failure('DesktopUnavailable', 'The selected desktop endpoint is absent or stale.');
    failure('UnsafeEndpoint', 'The collaboration descriptor could not be read safely.');
  } finally { await handle?.close(); }
}

export async function discover(dataRoot = defaultDataRoot(), signal) {
  let directory;
  try { directory = await descriptorDirectory(dataRoot); }
  catch (error) {
    if (error.code === 'ENOENT') return {instances: [], disclosure: DISCLOSURE};
    if (error instanceof CollaborationError) throw error;
    failure('DiscoveryUnavailable', 'The selected data root cannot be inspected.');
  }
  const names = [];
  let scanned = 0;
  for await (const entry of await opendir(directory)) {
    if (++scanned > 1024) failure('DiscoveryLimit', 'The collaboration directory exceeds its entry allowance.');
    if (!entry.name.endsWith('.json')) continue;
    names.push(entry.name.slice(0, -5));
    if (names.length > INSTANCE_LIMIT) failure('DiscoveryLimit', 'Too many instance descriptors; reconcile stale instances explicitly.');
  }
  const instances = [];
  for (const instance of names.sort()) {
    if (signal?.aborted) failure('Cancelled', 'Discovery was cancelled.');
    try {
      const endpoint = await readDescriptor(dataRoot, instance);
      const reply = await request(endpoint, null, null, {kind: 'describe'}, signal);
      instances.push({instance, protocol: PROTOCOL, available: reply.ok, ...(reply.ok ? reply.result : {error: reply.error})});
    } catch (error) {
      instances.push({instance, available: false, error: publicError(error)});
    }
  }
  return {instances, disclosure: DISCLOSURE};
}

export function publicError(error) {
  return error instanceof CollaborationError
    ? {...error.detail, code: error.code, message: error.message, outcome: error.outcome}
    : {code: 'AdapterFailure', message: 'The authoring request failed.', outcome: 'unknown'};
}

export async function request(endpoint, owner, packageIdentity, operation, signal) {
  if (signal?.aborted) failure('Cancelled', 'The request was cancelled before submission.');
  const current = await lstat(endpoint.socket).catch(() => null);
  if (!current || !sameFile(current, endpoint.socketIdentity)) failure('EndpointChanged', 'The selected endpoint changed; discover and select it again.');
  privateEntry(current, 'socket');
  const id = randomUUID();
  const body = Buffer.from(JSON.stringify({protocol: PROTOCOL, id, instance: endpoint.instance, token: endpoint.token, owner, package: packageIdentity, operation}));
  if (body.length > FRAME_BYTES) failure('PayloadLimit', 'The whole request exceeds the advertised transport limit.');
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  const mutation = !READ_ONLY.has(operation.kind);
  return new Promise((resolveReply, reject) => {
    let sent = false, settled = false;
    let cancellationTimer;
    const socket = connect({path: endpoint.socket});
    const prefix = Buffer.alloc(4);
    let prefixUsed = 0, payload = null, payloadUsed = 0;
    const unknown = () => sent && mutation ? 'unknown' : 'not_applied';
    const finish = (error, reply) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(cancellationTimer);
      signal?.removeEventListener('abort', cancel);
      socket.destroy();
      if (error) reject(error); else resolveReply(reply);
    };
    const cancel = () => {
      if (settled || cancellationTimer) return;
      if (!sent) { finish(new CollaborationError('Cancelled', 'The request was cancelled before submission.')); return; }
      const cancellation = Buffer.from(JSON.stringify({protocol: PROTOCOL, instance: endpoint.instance, token: endpoint.token, cancel: id}));
      const length = Buffer.alloc(4);
      length.writeUInt32BE(cancellation.length);
      socket.cork();
      socket.write(length);
      socket.write(cancellation);
      socket.uncork();
      cancellationTimer = setTimeout(() => finish(new CollaborationError('Cancelled', mutation
        ? 'Cancellation has no definitive result; outcome is unknown. Read current state before another mutation.'
        : 'The request was cancelled.', unknown())), 1000);
    };
    const timer = setTimeout(() => finish(new CollaborationError('ReplyUnavailable', sent && mutation
      ? 'The mutation reply was lost; outcome is unknown. Read current state before another mutation.'
      : 'The current desktop controller did not reply.', unknown())), 35_000);
    signal?.addEventListener('abort', cancel, {once: true});
    if (signal?.aborted) { cancel(); return; }
    socket.once('connect', () => {
      if (settled) return;
      sent = true;
      socket.cork();
      socket.write(header);
      socket.write(body);
      socket.uncork();
    });
    socket.on('data', bytes => {
      if (settled) return;
      let offset = 0;
      if (prefixUsed < 4) {
        const count = Math.min(4 - prefixUsed, bytes.length);
        bytes.copy(prefix, prefixUsed, 0, count);
        prefixUsed += count;
        offset += count;
        if (prefixUsed < 4) return;
        const length = prefix.readUInt32BE(0);
        if (length === 0 || length > FRAME_BYTES) {
          finish(new CollaborationError('FrameLimit', 'Desktop response exceeds the advertised frame bound.', unknown()));
          return;
        }
        payload = Buffer.alloc(length);
      }
      if (bytes.length - offset > payload.length - payloadUsed) {
        finish(new CollaborationError('InvalidReply', 'Desktop sent an invalid framed response.', unknown()));
        return;
      }
      bytes.copy(payload, payloadUsed, offset);
      payloadUsed += bytes.length - offset;
      if (payloadUsed !== payload.length) return;
      try {
        const reply = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(payload));
        const earlyRefusal = reply.id === null && reply.ok === false && reply.error?.outcome === 'not_applied'
          && ['busy', 'shutdown', 'unauthorized', 'unsupported_protocol', 'instance_mismatch', 'invalid_frame', 'frame_too_large'].includes(reply.error.code);
        if (reply.protocol !== PROTOCOL || (reply.id !== id && !earlyRefusal) || reply.instance !== endpoint.instance || typeof reply.ok !== 'boolean'
          || (reply.ok && reply.owner !== owner)) {
          throw new Error('correlation');
        }
        finish(null, reply);
      } catch { finish(new CollaborationError('InvalidReply', 'Desktop response correlation or encoding is invalid.', unknown())); }
    });
    socket.once('error', () => finish(new CollaborationError('DesktopUnavailable', sent && mutation
      ? 'The connection was lost after submission; outcome is unknown. Read current state before another mutation.'
      : 'The selected desktop endpoint is unavailable.', unknown())));
    socket.once('close', () => finish(new CollaborationError('ReplyUnavailable', 'The desktop closed the connection without a result.', unknown())));
  });
}

// The outcome a delivered reply establishes for a change.
function delivered(reply) {
  if (!reply.ok) return reply.error?.outcome === 'not_applied' ? 'not_applied' : 'unknown';
  if (reply.result?.failure?.outcome === 'unknown' || reply.result?.publication?.failure?.outcome === 'unknown') return 'unknown';
  return reply.result?.complete === false ? 'partial' : 'applied';
}

// Lets one caller stop waiting for a shared read-only request without cancelling it for the others.
function abandonable(promise, signal) {
  if (!signal) return promise;
  return new Promise((resolveValue, reject) => {
    const abandon = () => reject(new CollaborationError('Cancelled', 'The request was cancelled.'));
    if (signal.aborted) { abandon(); return; }
    signal.addEventListener('abort', abandon, {once: true});
    promise.then(resolveValue, reject).finally(() => signal.removeEventListener('abort', abandon));
  });
}

// One explicit connection: the selected endpoint, owner and package, and what must be observed before the next
// change. Nothing is retried, adopted from a reply or polled in the background.
export class AuthoringConnection {
  binding = null;
  pending = new Set();
  generation = 0;
  // Orders dispatches and observations: an observation counts only when dispatched after what it reconciles.
  #clock = 0;
  // Resources an unknown edit or Save may have changed: {since, settled}; settled once an idle context replied.
  #unknown = new Map();
  // Per fenced resource, the contiguous same-version page chain read since its settlement: {version, next}.
  #pages = new Map();
  // The clock of a package-wide change with an unknown outcome; any later idle context settles it.
  #global = null;
  // A lifecycle change with an unknown outcome; only an explicit connect clears it.
  #reconnect = false;
  // The default notice continuation {cursor, stamp} and the shared in-flight poll.
  #notice = null;
  #flight = null;

  disconnect() {
    this.generation += 1;
    this.binding = null;
    for (const controller of this.pending) controller.abort();
    this.pending.clear();
    this.#forget();
  }

  // What must be observed before the next change, or null.
  reconciliation() {
    if (!this.#reconnect && this.#global === null && this.#unknown.size === 0) return null;
    return {
      reconnect: this.#reconnect,
      context: this.#global !== null || [...this.#unknown.values()].some(fence => fence.settled === null),
      read: [...this.#unknown.keys()],
    };
  }

  async select({dataRoot = defaultDataRoot(), instance, owner, package: packageIdentity, acceptDisclosure}, signal) {
    if (acceptDisclosure !== true) failure('DisclosureRequired', DISCLOSURE);
    const owned = typeof owner === 'string' && typeof packageIdentity === 'string';
    if (!owned && (owner !== null || packageIdentity !== null)) {
      failure('InvalidSelection', 'Select both the Edit owner and package, or null for both when no package is open.');
    }
    this.disconnect();
    const generation = this.generation;
    const stamp = ++this.#clock;
    const controller = new AbortController();
    const cancel = () => controller.abort();
    this.pending.add(controller);
    signal?.addEventListener('abort', cancel, {once: true});
    if (signal?.aborted) controller.abort();
    try {
      const endpoint = await readDescriptor(dataRoot, instance);
      const response = await request(endpoint, null, null, {kind: 'describe'}, controller.signal);
      if (generation !== this.generation || controller.signal.aborted) failure('ConnectionChanged', 'Connection selection was cancelled or superseded.');
      if (!response.ok) return response;
      if (response.result?.owner !== owner || response.result?.package !== packageIdentity) {
        failure('OwnerMismatch', 'The selected Edit owner or package changed. Discover and explicitly select the intended owner.');
      }
      this.binding = {endpoint, owner, packageIdentity};
      this.#follow(response.result.cursor, stamp);
      return {...response, disclosure: DISCLOSURE};
    } finally {
      this.pending.delete(controller);
      signal?.removeEventListener('abort', cancel);
    }
  }

  async call(operation, signal) {
    const binding = this.binding;
    if (!binding) failure('NotConnected', 'Discover and explicitly connect to a desktop instance first.');
    if (this.pending.size >= ACTIVE_REQUESTS) failure('Busy', 'The adapter has reached its active request limit.');
    this.#admit(operation);
    const stamp = ++this.#clock;
    const controller = new AbortController();
    const cancel = () => controller.abort();
    this.pending.add(controller);
    signal?.addEventListener('abort', cancel, {once: true});
    if (signal?.aborted) controller.abort();
    try {
      const reply = await request(binding.endpoint, binding.owner, binding.packageIdentity, operation, controller.signal);
      if (this.binding !== binding) {
        // A delivered change result stays visible, but never touches the binding that replaced its own.
        if (READ_ONLY.has(operation.kind)) failure('ConnectionChanged', 'The selected connection changed while awaiting this result.');
        failure('ConnectionChanged', 'The selected connection changed while awaiting this result; the delivered reply does not apply to the current connection.',
          delivered(reply), {reply});
      }
      return this.#accept(binding, operation, reply, stamp);
    } catch (error) {
      const outcome = error instanceof CollaborationError ? error.outcome : 'unknown';
      if (outcome === 'unknown' && this.binding === binding) this.#uncertain(operation);
      throw error;
    } finally {
      this.pending.delete(controller);
      signal?.removeEventListener('abort', cancel);
    }
  }

  // An explicit poll; identical concurrent polls share one request. Without a cursor it continues from the latest
  // context or notices result of this connection, which advances only when a poll succeeds.
  async notices({cursor, limit} = {}, signal) {
    const binding = this.binding;
    if (!binding) failure('NotConnected', 'Discover and explicitly connect to a desktop instance first.');
    const from = cursor ?? this.#notice?.cursor ?? null;
    const key = JSON.stringify([from, limit ?? null]);
    let flight = this.#flight;
    if (flight?.binding !== binding || flight.key !== key) {
      const stamp = ++this.#clock;
      const controller = new AbortController();
      const operation = {kind: 'notices', ...(from === null ? {} : {cursor: from}), ...(limit === undefined ? {} : {limit})};
      const promise = this.call(operation, controller.signal).then(reply => {
        if (reply.ok && this.binding === binding) this.#follow(reply.result?.gap === 'owner' ? null : reply.result?.cursor, stamp);
        return reply;
      });
      const current = {binding, key, promise, controller, waiters: 0};
      this.#flight = flight = current;
      promise.catch(() => {}).finally(() => { if (this.#flight === current) this.#flight = null; });
    }
    flight.waiters += 1;
    try {
      return await abandonable(flight.promise, signal);
    } finally {
      if (--flight.waiters === 0) flight.controller.abort();
    }
  }

  #forget() {
    this.#unknown.clear();
    this.#pages.clear();
    this.#global = null;
    this.#reconnect = false;
    this.#notice = null;
    this.#flight = null;
  }

  // Reads, cancellation and refresh always pass, so reconciliation and refreshRequired recovery cannot deadlock.
  #admit(operation) {
    const {kind} = operation;
    if (READ_ONLY.has(kind) || RECOVERY.has(kind)) return;
    if (this.#reconnect) {
      failure('ReconnectRequired', 'A package lifecycle outcome is unknown. Discover and explicitly connect before another change.',
        'not_applied', {reconciliation: this.reconciliation()});
    }
    const touched = kind === 'edit' ? [...operation.edits, ...(operation.dependencies ?? [])].map(item => item.resource)
      : kind === 'save' ? [operation.resource] : null;
    if (this.#global !== null || (touched === null ? this.#unknown.size > 0 : touched.some(id => this.#unknown.has(id)))) {
      failure('ReconciliationRequired', 'An earlier change has an unknown outcome. Read context until nothing is pending, then read each listed resource completely, before this change.',
        'not_applied', {reconciliation: this.reconciliation()});
    }
  }

  // The change may or may not have happened; it is never retried, and dependent changes wait for observation.
  #uncertain(operation) {
    const {kind} = operation;
    if (READ_ONLY.has(kind)) return;
    const since = ++this.#clock;
    if (LIFECYCLE.has(kind)) this.#reconnect = true;
    else if (kind === 'edit' || kind === 'save') {
      for (const id of kind === 'edit' ? operation.edits.map(edit => edit.resource) : [operation.resource]) {
        this.#unknown.set(id, {since, settled: null});
        this.#pages.delete(id);
      }
    } else this.#global = since;
  }

  #accept(binding, operation, reply, stamp) {
    const {kind} = operation;
    if (!reply.ok) {
      if (reply.error?.outcome !== 'not_applied') this.#uncertain(operation);
    } else if (kind === 'describe') {
      const description = reply.result;
      if (description?.owner !== binding.owner || description?.package !== binding.packageIdentity) {
        failure('OwnerMismatch', 'The Edit owner or package changed. Discover and explicitly select the intended owner.');
      }
      this.#follow(description.cursor, stamp);
      if (description.pending == null && description.workerPending !== true) this.#settle(description, stamp);
    } else if (kind === 'read') this.#observe(operation.resource, reply.result, stamp);
    else if (LIFECYCLE.has(kind)) this.#move(binding, operation, reply);
    // A host exception without a commit receipt leaves the change as uncertain as a lost reply.
    else if (reply.result?.failure?.outcome === 'unknown') this.#uncertain(operation);
    const reconciliation = this.reconciliation();
    return reconciliation === null ? reply : {...reply, reconciliation};
  }

  // An idle context dispatched after an unknown outcome ends package-wide uncertainty; each affected resource is
  // then either gone or awaits a complete read dispatched after this reply.
  #settle(description, stamp) {
    if (this.#global !== null && this.#global < stamp) this.#global = null;
    if (!Array.isArray(description.resources)) return;
    const present = new Set(description.resources.map(item => item?.id));
    const settled = ++this.#clock;
    for (const [id, fence] of this.#unknown) {
      if (fence.since > stamp) continue;
      if (!present.has(id)) this.#unknown.delete(id);
      else fence.settled ??= settled;
    }
  }

  // Only contiguous pages of one version from offset 0 through nextOffset null are a complete observation.
  #observe(id, page, stamp) {
    const fence = this.#unknown.get(id);
    if (!fence || fence.settled === null) return;
    let progress = this.#pages.get(id);
    if (page?.offset === 0 && typeof page.version === 'string' && stamp > fence.settled) progress = {version: page.version, next: 0};
    if (!progress || page?.resource !== id || page.version !== progress.version || page.offset !== progress.next) {
      this.#pages.delete(id);
    } else if (page.nextOffset === null) {
      this.#unknown.delete(id);
      this.#pages.delete(id);
    } else if (Number.isSafeInteger(page.nextOffset) && page.nextOffset > page.offset) {
      this.#pages.set(id, {version: progress.version, next: page.nextOffset});
    } else this.#pages.delete(id);
  }

  // A lifecycle result binds the connection it names unless its own outcome is unknown; a different connection
  // starts a fresh lineage. An uncertain resolution Save still needs an idle context on whichever connection remains.
  #move(binding, operation, reply) {
    const result = reply.result;
    if (result?.failure?.outcome === 'unknown') {
      this.#uncertain(operation);
      return;
    }
    const owner = result?.connection?.owner, packageIdentity = result?.connection?.package;
    const owned = typeof owner === 'string' && IDENTITY.test(owner) && typeof packageIdentity === 'string' && IDENTITY.test(packageIdentity);
    if (!owned && (owner !== null || packageIdentity !== null)) {
      failure('InvalidReply', 'The lifecycle result names no valid connection. Discover and connect explicitly.', 'unknown', {reply});
    }
    if (owner !== binding.owner || packageIdentity !== binding.packageIdentity) {
      this.binding = {endpoint: binding.endpoint, owner, packageIdentity};
      this.#forget();
    }
    if (result.publication?.failure?.outcome === 'unknown') this.#global = ++this.#clock;
  }

  #follow(cursor, stamp) {
    if (this.#notice !== null && this.#notice.stamp > stamp) return;
    this.#notice = {cursor: typeof cursor === 'string' ? cursor : null, stamp};
  }
}

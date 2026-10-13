import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:net';
import {chmod, mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {once} from 'node:events';
import {spawn} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {AuthoringConnection, checkCompatibility, discover, FRAME_BYTES, publicError, readDescriptor, request} from './client.mjs';
import register from './index.mjs';

const WINDOWS = process.platform === 'win32';
const hostApi = {registerTool() {}, registerCommand() {}, on() {}, zod: {object() {}}};
const versions = [
  {scenario: 'rejects an older OMP minor', version: '18.7.99', code: 'OmpVersionTooOld'},
  {scenario: 'rejects a prerelease below the stable minimum', version: '18.8.7-beta.1', code: 'OmpVersionTooOld'},
  {scenario: 'rejects an unavailable runtime version', version: undefined, code: 'OmpVersionUnavailable'},
];
for (const {scenario, version, code} of versions) test(scenario, () => {
  assert.throws(() => checkCompatibility(version, hostApi), {code});
});
test('reports a missing required capability independently of a newer OMP version', () => {
  assert.throws(() => checkCompatibility('25.1.3', {...hostApi, on: undefined}), {code: 'OmpCapabilityMissing'});
});

function frame(value) {
  const body = Buffer.from(JSON.stringify(value));
  const size = Buffer.alloc(4);
  size.writeUInt32BE(body.length);
  return Buffer.concat([size, body]);
}

// Only newly owned fixture paths/pipe ACLs are changed. Production privacy checks
// still inspect real Windows security descriptors; these are not bypassed.
async function windowsFixtureAcl(paths, pipe, extraUser = false) {
  const script = String.raw`
    $ErrorActionPreference = 'Stop'
    $config = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $user = $identity.User
    foreach ($path in $config.paths) {
      $directory = [IO.Directory]::Exists($path)
      $acl = if ($directory) { [Security.AccessControl.DirectorySecurity]::new() } else { [Security.AccessControl.FileSecurity]::new() }
      $existing = if ($directory) { [IO.Directory]::GetAccessControl($path) } else { [IO.File]::GetAccessControl($path) }
      $owner = $existing.GetOwner([Security.Principal.SecurityIdentifier])
      if (-not $owner.Equals($user) -and -not $owner.Equals($identity.Owner)) { throw 'fixture owner mismatch' }
      # Elevated hosted runners may default newly created objects to their owner
      # group. Assign only our fresh fixtures to the exact test user.
      if (-not $owner.Equals($user)) { $acl.SetOwner($user) }
      $acl.SetAccessRuleProtection($true, $false)
      $rule = if ($directory) {
        [Security.AccessControl.FileSystemAccessRule]::new($user, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
      } else { [Security.AccessControl.FileSystemAccessRule]::new($user, 'FullControl', 'Allow') }
      $acl.AddAccessRule($rule)
      if ($config.extraUser) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-1-0'), 'Read', 'Allow'))
      }
      if ($directory) { [IO.Directory]::SetAccessControl($path, $acl) } else { [IO.File]::SetAccessControl($path, $acl) }
      $actual = if ($directory) { [IO.Directory]::GetAccessControl($path) } else { [IO.File]::GetAccessControl($path) }
      if (-not $actual.GetOwner([Security.Principal.SecurityIdentifier]).Equals($user)) { throw 'fixture owner assignment failed' }
    }
    if ($config.pipe) {
      $rights = [IO.Pipes.PipeAccessRights]::ReadWrite -bor [IO.Pipes.PipeAccessRights]::ChangePermissions
      if (-not $identity.Owner.Equals($user)) { $rights = $rights -bor [IO.Pipes.PipeAccessRights]::TakeOwnership }
      $client = [IO.Pipes.NamedPipeClientStream]::new('.', $config.pipe.Substring(9), $rights, [IO.Pipes.PipeOptions]::None,
        [Security.Principal.TokenImpersonationLevel]::Identification, [IO.HandleInheritability]::None)
      try {
        $client.Connect(5000)
        $acl = [IO.Pipes.PipeSecurity]::new()
        $owner = $client.GetAccessControl().GetOwner([Security.Principal.SecurityIdentifier])
        if (-not $owner.Equals($user) -and -not $owner.Equals($identity.Owner)) { throw 'fixture pipe owner mismatch' }
        if (-not $owner.Equals($user)) { $acl.SetOwner($user) }
        $acl.SetAccessRuleProtection($true, $false)
        $acl.AddAccessRule([IO.Pipes.PipeAccessRule]::new($user, 'FullControl', 'Allow'))
        $client.SetAccessControl($acl)
        if (-not $client.GetAccessControl().GetOwner([Security.Principal.SecurityIdentifier]).Equals($user)) { throw 'fixture pipe owner assignment failed' }
      } finally { $client.Dispose() }
    }
  `;
  const child = spawn(join(process.env.SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
    ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', script], {windowsHide: true, stdio: ['pipe', 'ignore', 'pipe']});
  let error = '';
  child.stderr.on('data', bytes => { error += bytes.toString(); });
  const timer = setTimeout(() => child.kill(), 10_000);
  child.stdin.end(JSON.stringify({paths, pipe, extraUser}));
  try { assert.equal((await once(child, 'close'))[0], 0, error); }
  finally { clearTimeout(timer); }
}
async function peer(t, handler) {
  const root = await mkdtemp(join(await realpath(tmpdir()), 'mc-'));
  const directory = join(root, 'collaboration');
  await mkdir(directory, {mode: 0o700});
  const instance = randomBytes(16).toString('hex');
  const path = WINDOWS ? `\\\\.\\pipe\\mado-mata-collaboration-${instance}` : join(root, 's');
  const sockets = new Set();
  const server = createServer(socket => {
    sockets.add(socket);
    socket.on('error', () => {});
    socket.on('close', () => sockets.delete(socket));
    let input = Buffer.alloc(0);
    socket.on('data', chunk => {
      input = Buffer.concat([input, chunk]);
      while (input.length >= 4 && input.length >= 4 + input.readUInt32BE()) {
        const count = input.readUInt32BE();
        const value = JSON.parse(input.subarray(4, 4 + count));
        input = input.subarray(4 + count);
        handler(value, socket, result => socket.write(frame({protocol: 1, instance, id: value.id, owner: value.owner, ...result})));
      }
    });
  });
  t.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    await rm(root, {recursive: true, force: true});
  });
  server.listen(path);
  await once(server, 'listening');
  if (!WINDOWS) await chmod(path, 0o600);
  const descriptor = {protocol: 1, instance, socket: path, token: 'a'.repeat(64), pid: process.pid};
  const descriptorPath = join(directory, `${instance}.json`);
  await writeFile(descriptorPath, JSON.stringify(descriptor), {mode: 0o600});
  if (WINDOWS) await windowsFixtureAcl([root, directory, descriptorPath], path);
  return {root, descriptorPath, path, endpoint: await readDescriptor(root, descriptor.instance)};
}
// A scripted desktop recording each request; cancellation frames are ignored.
async function desktop(t, respond) {
  const requests = [];
  const fixture = await peer(t, (value, socket, reply) => {
    if (value.cancel !== undefined) return;
    requests.push(value);
    respond(value.operation, reply, socket);
  });
  return {...fixture, requests, kinds: () => requests.map(value => value.operation.kind)};
}
// A request the desktop holds: `held` resolves with what the handler passes to `hold`.
function holder() {
  let hold;
  const held = new Promise(resolve => { hold = resolve; });
  return {held, hold};
}
const describe = {owner: 'owner-a', package: 'package-a', selected: 'file-a', resources: [{id: 'file-a', version: 'v1'}], pending: null, cursor: 'c1'};
const ownerless = {owner: null, package: null, selected: null, resources: [], pending: null, cursor: null};
const selection = fixture => ({dataRoot: fixture.root, instance: fixture.endpoint.instance, owner: 'owner-a', package: 'package-a', acceptDisclosure: true});
const partial = {complete: false, committed: [{path: 'main.ts', revision: 'rev-2'}], remaining: [{resource: 'file-b', version: 'v1'}],
  savedRevision: 'rev-2', refreshRequired: true, failure: {code: 'host_io', message: 'The package view could not be read'}};

const unsafeDescriptors = [
  {scenario: 'refuses a group-readable descriptor before connecting', makeUnsafe: async path => chmod(path, 0o640)},
  {scenario: 'refuses a descriptor symlink before connecting', makeUnsafe: async path => {
    const original = await readFile(path);
    await writeFile(`${path}.target`, original, {mode: 0o600});
    await rm(path);
    await symlink(`${path}.target`, path);
  }},
];
for (const {scenario, makeUnsafe} of unsafeDescriptors) test(scenario, {skip: WINDOWS}, async t => {
  let calls = 0;
  const fixture = await peer(t, () => { calls += 1; });
  await makeUnsafe(fixture.descriptorPath);
  await assert.rejects(readDescriptor(fixture.root, fixture.endpoint.instance), {code: 'UnsafeEndpoint'});
  assert.equal(calls, 0);
});

test('discovery of a missing selected root remains empty', async t => {
  const root = await mkdtemp(join(await realpath(tmpdir()), 'mc-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  assert.deepEqual((await discover(join(root, 'absent'))).instances, []);
});

test('Windows accepts a read-only shared root but rejects a readable descriptor ACL', {skip: !WINDOWS}, async t => {
  let calls = 0;
  const fixture = await peer(t, () => { calls += 1; });
  await windowsFixtureAcl([fixture.root], null, true);
  assert.equal((await readDescriptor(fixture.root, fixture.endpoint.instance)).instance, fixture.endpoint.instance);
  await windowsFixtureAcl([fixture.descriptorPath], null, true);
  await assert.rejects(readDescriptor(fixture.root, fixture.endpoint.instance), {code: 'UnsafeEndpoint'});
  assert.equal(calls, 0);
});

test('Windows rejects a junction in the selected endpoint path', {skip: !WINDOWS}, async t => {
  const fixture = await peer(t, () => assert.fail('no request may be submitted'));
  const alias = join(fixture.root, 'alias');
  await symlink(fixture.root, alias, 'junction');
  await assert.rejects(readDescriptor(alias, fixture.endpoint.instance), {code: 'UnsafeEndpoint'});
});

test('Windows rejects remote pipes and descriptor replacement before submission', {skip: !WINDOWS}, async t => {
  let calls = 0;
  const fixture = await peer(t, () => { calls += 1; });
  const descriptor = JSON.parse(await readFile(fixture.descriptorPath));
  await writeFile(fixture.descriptorPath, JSON.stringify({...descriptor, socket: '\\\\remote\\pipe\\mado-mata-collaboration-' + descriptor.instance}));
  await assert.rejects(readDescriptor(fixture.root, descriptor.instance), {code: 'UnsafeEndpoint'});
  await rename(fixture.descriptorPath, fixture.descriptorPath + '.old');
  await writeFile(fixture.descriptorPath, JSON.stringify(descriptor));
  await windowsFixtureAcl([fixture.descriptorPath], null);
  await assert.rejects(request(fixture.endpoint, 'owner-a', 'package-a', {kind: 'save_all'}), {code: 'EndpointChanged', outcome: 'not_applied'});
  assert.equal(calls, 0);
});

test('Windows checks the actual connected pipe server PID before sending a frame', {skip: !WINDOWS}, async t => {
  let calls = 0;
  const fixture = await peer(t, () => { calls += 1; });
  const descriptor = JSON.parse(await readFile(fixture.descriptorPath));
  await writeFile(fixture.descriptorPath, JSON.stringify({...descriptor, pid: process.pid + 1}));
  const wrong = await readDescriptor(fixture.root, descriptor.instance);
  await assert.rejects(request(wrong, 'owner-a', 'package-a', {kind: 'save_all'}), {code: 'EndpointChanged', outcome: 'not_applied'});
  assert.equal(calls, 0);
});

test('refuses an oversized declared response before reading its body', async t => {
  const fixture = await peer(t, (_request, socket) => {
    const header = Buffer.alloc(4); header.writeUInt32BE(FRAME_BYTES + 1); socket.write(header);
  });
  await assert.rejects(request(fixture.endpoint, null, null, {kind: 'describe'}), {code: 'FrameLimit', outcome: 'not_applied'});
});

test('an unknown edit is reconciled only by an idle context and then one complete same-version page chain', async t => {
  const draft = {pending: 'save', version: 'v2', text: 'committed draft'};
  let edits = 0;
  const fixture = await desktop(t, (operation, reply, socket) => {
    if (operation.kind === 'describe') reply({ok: true, result: {...describe, pending: draft.pending}});
    if (operation.kind === 'read') {
      const offset = operation.offset ?? 0;
      const end = Math.min(draft.text.length, offset + (operation.limit ?? 64));
      reply({ok: true, result: {resource: operation.resource, version: draft.version, text: draft.text.slice(offset, end), offset,
        nextOffset: end < draft.text.length ? end : null, totalUnits: draft.text.length}});
    }
    if (operation.kind === 'edit') {
      edits += 1;
      if (edits === 1) socket.destroy();
      else reply({ok: true, result: {resources: [{resource: 'file-a', version: 'v4'}], created: [], savedRevision: 'rev-1'}});
    }
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  await assert.rejects(connection.call({kind: 'edit', edits: [{resource: 'file-a', version: 'v1', text: 'committed draft'}]}), {outcome: 'unknown'});
  const read = (offset, limit) => connection.call({kind: 'read', resource: 'file-a', offset, limit});
  const fenced = () => assert.rejects(connection.call({kind: 'edit', edits: [{resource: 'file-a', version: draft.version, text: 'next'}]}),
    {code: 'ReconciliationRequired', outcome: 'not_applied'});
  await read(0, 64);
  await fenced();
  await connection.call({kind: 'describe'});
  await read(0, 64);
  await fenced();
  draft.pending = null;
  await connection.call({kind: 'describe'});
  await read(0, 8);
  await fenced();
  // A person typed between pages: the chain must restart at offset 0 for the new version.
  draft.version = 'v3';
  await read(8, 64);
  await fenced();
  await read(0, 8);
  assert.equal((await read(8, 64)).result.nextOffset, null);
  assert.equal(connection.reconciliation(), null);
  assert.equal((await connection.call({kind: 'edit', edits: [{resource: 'file-a', version: 'v3', text: 'next'}]})).ok, true);
  assert.equal(edits, 2);
});

test('an unknown edit fences its targets, dependents and package-wide changes but not unrelated drafts; a removed target needs no read', async t => {
  const resources = [{id: 'file-a', version: 'v1'}, {id: 'file-b', version: 'v1'}];
  let lost = true;
  const fixture = await desktop(t, (operation, reply, socket) => {
    if (operation.kind === 'describe') reply({ok: true, result: {...describe, resources}});
    else if (operation.kind === 'edit' && lost) { lost = false; socket.destroy(); }
    else if (operation.kind === 'edit') reply({ok: true, result: {resources: [{resource: 'file-a', version: 'v2'}], created: [], savedRevision: 'rev-1'}});
    else reply({ok: true, result: {complete: true, committed: [], remaining: [], savedRevision: 'rev-1', refreshRequired: false, failure: null}});
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  await assert.rejects(connection.call({kind: 'edit', edits: [{resource: 'file-b', version: 'v1', text: 'B'}]}), {outcome: 'unknown'});
  assert.equal((await connection.call({kind: 'edit', edits: [{resource: 'file-a', version: 'v1', text: 'A'}]})).ok, true);
  await assert.rejects(connection.call({kind: 'edit', edits: [{resource: 'file-a', version: 'v2', text: 'A2'}], dependencies: [{resource: 'file-b', version: 'v1'}]}),
    {code: 'ReconciliationRequired'});
  await assert.rejects(connection.call({kind: 'save_all'}), {code: 'ReconciliationRequired'});
  resources.splice(1, 1);
  await connection.call({kind: 'describe'});
  assert.equal(connection.reconciliation(), null);
  assert.equal((await connection.call({kind: 'save_all'})).ok, true);
  assert.deepEqual(fixture.kinds(), ['describe', 'edit', 'edit', 'describe', 'save_all']);
});

test('a lost Save all blocks further publication until an idle context, while refresh and cancel stay available', async t => {
  const host = {pending: null};
  const fixture = await desktop(t, (operation, reply, socket) => {
    if (operation.kind === 'describe') reply({ok: true, result: {...describe, pending: host.pending}});
    else if (operation.kind === 'save_all') socket.destroy();
    else if (operation.kind === 'refresh') reply({ok: true, result: {complete: true, savedRevision: 'rev-1', refreshRequired: false}});
    else if (operation.kind === 'cancel') reply({ok: true, result: {requested: true, settled: false}});
    else reply({ok: true, result: {complete: true, committed: [{path: 'main.ts', revision: 'rev-2'}], remaining: [], savedRevision: 'rev-2', refreshRequired: false, failure: null}});
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  await assert.rejects(connection.call({kind: 'save_all'}), {outcome: 'unknown'});
  const save = {kind: 'save', resource: 'file-a', version: 'v1'};
  await assert.rejects(connection.call(save), error => error.code === 'ReconciliationRequired' && error.detail.reconciliation.context === true);
  assert.equal((await connection.call({kind: 'refresh'})).ok, true);
  assert.equal((await connection.call({kind: 'cancel'})).ok, true);
  host.pending = 'validate';
  await connection.call({kind: 'describe'});
  await assert.rejects(connection.call(save), {code: 'ReconciliationRequired'});
  host.pending = null;
  await connection.call({kind: 'describe'});
  assert.equal((await connection.call(save)).ok, true);
  assert.deepEqual(fixture.kinds(), ['describe', 'save_all', 'refresh', 'cancel', 'describe', 'describe', 'save']);
});

test('a Save reporting an unknown host failure fences its resource like a lost reply', async t => {
  const fixture = await desktop(t, (operation, reply) => reply({ok: true, result: operation.kind === 'describe' ? describe
    : {...partial, committed: [], remaining: [{resource: 'file-a', version: 'v1'}], refreshRequired: false, failure: {code: 'host_io', message: 'Save failed', outcome: 'unknown'}}}));
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const saved = await connection.call({kind: 'save', resource: 'file-a', version: 'v1'});
  assert.deepEqual(saved.reconciliation, {reconnect: false, context: true, read: ['file-a']});
  await assert.rejects(connection.call({kind: 'save', resource: 'file-a', version: 'v1'}), {code: 'ReconciliationRequired'});
  assert.deepEqual(fixture.kinds(), ['describe', 'save']);
});

test('a Save all delivered after the connection changed stays visible as partial, not unapplied', async t => {
  const {held, hold} = holder();
  const fixture = await desktop(t, (operation, reply) => {
    if (operation.kind === 'describe') reply({ok: true, result: describe});
    else hold(() => reply({ok: true, result: partial}));
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const saving = connection.call({kind: 'save_all'});
  const release = await held;
  const reselecting = connection.select(selection(fixture));
  release();
  const error = await saving.then(() => null, caught => caught);
  await reselecting;
  const shown = publicError(error);
  assert.equal(shown.code, 'ConnectionChanged');
  assert.equal(shown.outcome, 'partial');
  assert.deepEqual(shown.reply.result, partial);
  assert.equal(connection.reconciliation(), null);
});

test('no-owner selection needs both nulls, and only an explicit lifecycle result moves the binding', async t => {
  let current = ownerless;
  const fixture = await desktop(t, (operation, reply) => {
    if (operation.kind === 'describe') reply({ok: true, result: current});
    else {
      current = {...describe, owner: 'owner-b', package: 'package-b'};
      reply({ok: true, result: {complete: true, connection: {owner: 'owner-b', package: 'package-b'}, previousReleased: false, failure: null, publication: null}});
    }
  });
  const connection = new AuthoringConnection();
  await assert.rejects(connection.select({...selection(fixture), owner: null}), {code: 'InvalidSelection', outcome: 'not_applied'});
  assert.equal(fixture.requests.length, 0);
  await assert.rejects(connection.select(selection(fixture)), {code: 'OwnerMismatch'});
  await connection.select({...selection(fixture), owner: null, package: null});
  await connection.call({kind: 'create', workspace: 'workspace-a', packageId: 'example.b'});
  assert.equal((await connection.call({kind: 'describe'})).result.owner, 'owner-b');
  assert.deepEqual(fixture.requests.map(value => [value.operation.kind, value.owner, value.package]),
    [['describe', null, null], ['describe', null, null], ['create', null, null], ['describe', 'owner-b', 'package-b']]);
});

test('a lost lifecycle result requires an explicit reconnect and never adopts the successor owner', async t => {
  let current = describe;
  const fixture = await desktop(t, (operation, reply, socket) => {
    if (operation.kind === 'describe') reply({ok: true, result: current});
    else { current = ownerless; socket.destroy(); }
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  await assert.rejects(connection.call({kind: 'exit', resolution: 'discard'}), {outcome: 'unknown'});
  await assert.rejects(connection.call({kind: 'edit', edits: [{resource: 'file-a', version: 'v1', text: 'x'}]}), {code: 'ReconnectRequired'});
  await assert.rejects(connection.call({kind: 'describe'}), {code: 'OwnerMismatch'});
  assert.deepEqual(connection.reconciliation(), {reconnect: true, context: false, read: []});
  await connection.select({...selection(fixture), owner: null, package: null});
  assert.equal(connection.reconciliation(), null);
  assert.deepEqual(fixture.requests.map(value => [value.operation.kind, value.owner]),
    [['describe', null], ['exit', 'owner-a'], ['describe', 'owner-a'], ['describe', null]]);
});

test('a lifecycle result with an unknown outcome requires reconnect even when it names a connection', async t => {
  const fixture = await desktop(t, (operation, reply) => reply({ok: true, result: operation.kind === 'describe' ? describe
    : {complete: false, connection: {owner: 'owner-b', package: 'package-b'}, previousReleased: true, failure: {code: 'host_io', message: 'Open failed', outcome: 'unknown'}, publication: null}}));
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const opened = await connection.call({kind: 'open', workspace: 'workspace-a', path: '/packages/b'});
  assert.equal(opened.reconciliation.reconnect, true);
  await assert.rejects(connection.call({kind: 'save_all'}), {code: 'ReconnectRequired'});
  await connection.call({kind: 'describe'});
  assert.equal(fixture.requests.at(-1).owner, 'owner-a');
});

test('a lifecycle result delivered after reselection never moves the new binding', async t => {
  const {held, hold} = holder();
  const fixture = await desktop(t, (operation, reply) => {
    if (operation.kind === 'describe') reply({ok: true, result: describe});
    else hold(() => reply({ok: true, result: {complete: true, connection: {owner: 'owner-b', package: 'package-b'}, previousReleased: true, failure: null, publication: null}}));
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const opening = connection.call({kind: 'open', workspace: 'workspace-a', path: '/packages/b'});
  const release = await held;
  const reselecting = connection.select(selection(fixture));
  release();
  const error = await opening.then(() => null, caught => caught);
  await reselecting;
  assert.equal(publicError(error).outcome, 'applied');
  await connection.call({kind: 'describe'});
  assert.equal(fixture.requests.at(-1).owner, 'owner-a');
});

test('notices continue from the latest context, keep their cursor when a poll fails, and never adopt another owner', async t => {
  const ownerGap = reply => reply({ok: true, result: {cursor: null, gap: 'owner', changes: [], more: false, savedRevision: null}});
  const polls = [
    reply => reply({ok: true, result: {cursor: 'c2', gap: null, changes: [{resource: 'file-a', path: 'main.ts', kind: 'source', version: 'v2'}], more: false, savedRevision: 'rev-1'}}),
    (_reply, socket) => socket.destroy(),
    ownerGap,
    ownerGap,
  ];
  const fixture = await desktop(t, (operation, reply, socket) => {
    if (operation.kind === 'describe') reply({ok: true, result: describe});
    else polls.shift()(reply, socket);
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  assert.equal((await connection.notices()).result.changes[0].version, 'v2');
  await assert.rejects(connection.notices(), {outcome: 'not_applied'});
  assert.equal((await connection.notices()).result.gap, 'owner');
  await connection.notices();
  const sent = fixture.requests.filter(value => value.operation.kind === 'notices');
  assert.deepEqual(sent.map(value => value.operation.cursor), ['c1', 'c2', 'c2', undefined]);
  assert.deepEqual(sent.map(value => value.owner), ['owner-a', 'owner-a', 'owner-a', 'owner-a']);
});

test('identical concurrent notice polls share one desktop request', async t => {
  const {held, hold} = holder();
  const fixture = await desktop(t, (operation, reply) => {
    if (operation.kind === 'describe') reply({ok: true, result: describe});
    else hold(() => reply({ok: true, result: {cursor: 'c2', gap: null, changes: [], more: false, savedRevision: 'rev-1'}}));
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const polls = [connection.notices(), connection.notices()];
  (await held)();
  assert.deepEqual((await Promise.all(polls)).map(reply => reply.result.cursor), ['c2', 'c2']);
  assert.deepEqual(fixture.kinds(), ['describe', 'notices']);
});

test('cancellation before server claim receives a definitive not-applied result', async t => {
  let admitted;
  let notifyRequest;
  const received = new Promise(resolve => { notifyRequest = resolve; });
  const fixture = await peer(t, (value, socket) => {
    if (value.cancel) socket.write(frame({protocol: 1, instance: value.instance, id: value.cancel, owner: null, ok: false, error: {code: 'cancelled', outcome: 'not_applied'}}));
    else { admitted = value.id; notifyRequest(); }
  });
  const abort = new AbortController();
  const pending = request(fixture.endpoint, 'owner-a', 'package-a', {kind: 'edit', edits: [{resource: 'file-a', version: 'v1', text: 'new'}]}, abort.signal);
  await received;
  abort.abort();
  const result = await pending;
  assert.equal(result.id, admitted);
  assert.equal(result.ok, false);
  assert.equal(result.error.outcome, 'not_applied');
});

test('session teardown cannot adopt an in-flight connection selection', async t => {
  let release;
  const received = new Promise(resolve => { release = resolve; });
  const fixture = await peer(t, (_value, _socket, reply) => release(() => reply({ok: true, result: describe})));
  const connection = new AuthoringConnection();
  const selecting = connection.select(selection(fixture));
  const respond = await received;
  connection.disconnect();
  respond();
  await assert.rejects(selecting, {code: 'ConnectionChanged'});
  await assert.rejects(connection.call({kind: 'describe'}), {code: 'NotConnected'});
});

test('a context read never adopts a successor Edit owner', async t => {
  let snapshots = 0;
  const fixture = await peer(t, (_value, _socket, reply) => {
    snapshots += 1;
    reply({ok: true, result: snapshots === 1 ? describe : {...describe, owner: 'owner-b'}});
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  await assert.rejects(connection.call({kind: 'describe'}), {code: 'OwnerMismatch'});
  assert.equal(connection.binding.owner, 'owner-a');
});

test('parallel requests above the adapter allowance are refused before submission', async t => {
  const waiting = [];
  let allReceived;
  const received = new Promise(resolve => { allReceived = resolve; });
  const fixture = await peer(t, (value, _socket, reply) => {
    if (value.operation.kind === 'describe') { reply({ok: true, result: describe}); return; }
    waiting.push(reply);
    if (waiting.length === 4) allReceived();
  });
  const connection = new AuthoringConnection();
  await connection.select(selection(fixture));
  const pending = Array.from({length: 4}, () => connection.call({kind: 'read', resource: 'file-a'}));
  await received;
  await assert.rejects(connection.call({kind: 'read', resource: 'file-a'}), {code: 'Busy', outcome: 'not_applied'});
  assert.equal(waiting.length, 4);
  for (const reply of waiting) reply({ok: true, result: {resource: 'file-a', text: 'live', version: 'v1'}});
  await Promise.all(pending);
});

test('a connection-cap refusal is not confused with a lost mutation result', async t => {
  const fixture = await peer(t, (value, socket) => socket.write(frame({protocol: 1, id: null, instance: value.instance, owner: null,
    ok: false, error: {code: 'busy', message: 'Connection limit', outcome: 'not_applied'}})));
  const result = await request(fixture.endpoint, 'owner-a', 'package-a', {kind: 'edit', edits: [{resource: 'file-a', version: 'v1', text: 'new'}]});
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'busy');
  assert.equal(result.error.outcome, 'not_applied');
});

// The extension against OMP's observed factory API; the schema stub accepts every declared shape unchanged.
function omp() {
  const tools = new Map(), events = new Map(), messages = [];
  let command;
  const schema = new Proxy(function schema() {}, {get: (_target, key) => key === 'parse' ? value => value : schema, apply: () => schema});
  register({pi: {VERSION: '18.8.7'}, zod: schema, registerTool: tool => tools.set(tool.name, tool),
    registerCommand: (_name, definition) => { command = definition; }, on: (event, handler) => events.set(event, handler),
    sendMessage: (message, options) => messages.push({message, options})});
  return {
    run: (name, params, signal) => tools.get(`mado_${name}`).execute('call', params, signal),
    emit: event => events.get(event)(),
    command: args => command.handler(args, {ui: {notify() {}}}),
    messages,
  };
}

test('a partial Save all is a tool error that keeps its committed prefix', async t => {
  const fixture = await desktop(t, (operation, reply) => reply({ok: true, result: operation.kind === 'describe' ? describe : partial}));
  const extension = omp();
  await extension.run('connect', selection(fixture));
  const saved = await extension.run('save_all', {});
  assert.equal(saved.isError, true);
  assert.deepEqual(saved.details.result, partial);
});

test('a session switch cancels in-flight work, withholds its result and clears the connection', async t => {
  const {held, hold} = holder();
  const cancelled = [];
  const fixture = await peer(t, (value, socket, reply) => {
    if (value.cancel !== undefined) {
      cancelled.push(value.cancel);
      socket.write(frame({protocol: 1, instance: value.instance, id: value.cancel, owner: null, ok: false, error: {code: 'cancelled', message: 'Cancelled', outcome: 'not_applied'}}));
    } else if (value.operation.kind === 'describe') reply({ok: true, result: describe});
    else hold(value.id);
  });
  const extension = omp();
  await extension.run('connect', selection(fixture));
  const reading = extension.run('read', {resource: 'file-a'});
  const id = await held;
  extension.emit('session_switch');
  const withheld = await reading;
  assert.equal(withheld.details.error.code, 'SessionChanged');
  assert.equal(withheld.details.error.outcome, 'not_applied');
  assert.deepEqual(cancelled, [id]);
  assert.equal((await extension.run('context', {})).details.error.code, 'NotConnected');
});

test('/mado shares the tool connection and never starts a model turn', async t => {
  const fixture = await desktop(t, (_operation, reply) => reply({ok: true, result: describe}));
  const extension = omp();
  await extension.command(`connect ${JSON.stringify(selection(fixture))}`);
  await extension.run('context', {});
  await extension.command('context {}');
  assert.deepEqual(extension.messages.map(entry => entry.options), [{triggerTurn: false}, {triggerTurn: false}]);
  assert.deepEqual(fixture.requests.map(value => value.owner), [null, 'owner-a', 'owner-a']);
});

// The subset of OMP's zod builder the extension declares with, validating as zod does: objects strip unknown members
// unless strict, optional members may be absent, and unions accept the first matching branch.
function zodSubset() {
  const schema = check => ({
    check,
    parse(value) { if (!check(value)) throw new Error('invalid'); return value; },
    min: limit => schema(value => check(value) && (typeof value === 'number' ? value >= limit : value.length >= limit)),
    max: limit => schema(value => check(value) && (typeof value === 'number' ? value <= limit : value.length <= limit)),
    int: () => schema(value => check(value) && Number.isInteger(value)),
    nullable: () => schema(value => value === null || check(value)),
    optional: () => schema(value => value === undefined || check(value)),
    describe() { return this; },
  });
  const object = (shape, strict) => ({
    ...schema(value => value !== null && typeof value === 'object' && !Array.isArray(value)
      && Object.entries(shape).every(([key, item]) => item.check(value[key])) && (!strict || Object.keys(value).every(key => Object.hasOwn(shape, key)))),
    strict: () => object(shape, true),
  });
  return {
    object: shape => object(shape, false),
    string: () => schema(value => typeof value === 'string'),
    number: () => schema(value => typeof value === 'number' && Number.isFinite(value)),
    boolean: () => schema(value => typeof value === 'boolean'),
    unknown: () => schema(() => true),
    literal: expected => schema(value => value === expected),
    enum: values => schema(value => values.includes(value)),
    array: item => schema(value => Array.isArray(value) && value.every(entry => item.check(entry))),
    union: options => schema(value => options.some(option => option.check(value))),
  };
}

test('the edit tool schema admits the distribution manifest fields in their exact shapes and offers no tag operation', () => {
  const tools = new Map();
  register({pi: {VERSION: '18.8.7'}, zod: zodSubset(), registerTool: tool => tools.set(tool.name, tool), registerCommand() {}, on() {}});
  const edit = fields => tools.get('mado_edit').parameters.check({edits: [{resource: 'manifest-a', version: 'v1', fields}]});
  assert.equal(edit([{field: 'distribution', declared: true}, {field: 'distribution', part: 'game_id', value: 'example-game'},
    {field: 'distribution', part: 'release_version', value: '1.2.0-beta.1'}, {field: 'gameName', key: 'zh-Hant', name: '範例'},
    {field: 'gameName', key: 'ja', name: null}, {field: 'gameAliases', aliases: ['EX', 'Example.exe']}]), true);
  for (const field of [
    {field: 'distribution'}, {field: 'distribution', declared: true, part: 'name', value: 'x'}, {field: 'distribution', part: 'tags', value: 'x'},
    {field: 'distribution', part: 'version', value: '1'}, {field: 'gameName', key: 'ja'}, {field: 'gameName', key: 'ja', name: 1},
    {field: 'gameAliases', aliases: Array(65).fill('x')}, {field: 'gameAliases', aliases: [1]}, {field: 'tags', tags: ['internal']},
  ]) assert.equal(edit([field]), false, JSON.stringify(field));
});

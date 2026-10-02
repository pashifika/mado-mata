import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import ts from 'typescript';
import {trustedLibraries} from '../../build/trusted-libraries.mjs';
import {definitions} from '../../../../tools/runtime-comparison/compiler/sdk.mjs';
import {AUTHORING_SOURCE_BYTES} from '../authoring.ts';
import {CANDIDATE_LIMIT, DETAIL_BYTES, RESPONSE_BYTES} from './completion-types.ts';
import {ScriptLanguageService} from './language-service.ts';

const libraries = trustedLibraries();
const schema = {
  version: 1, type: 'object', additionalProperties: false, required: ['settings'],
  properties: {
    settings: {
      type: 'object', additionalProperties: false, required: ['mode', 'threshold'],
      properties: {mode: {type: 'string', enum: ['fast', 'careful']}, threshold: {type: 'number'}},
    },
  },
};
const schemaText = JSON.stringify(schema);
const compilerPath = fileURLToPath(new URL('../../../../tools/runtime-comparison/compiler/compile.mjs', import.meta.url));
let serial = 0;

function request(source, path = 'main.ts', currentSchema = schemaText) {
  return {kind: 'complete', key: {owner: 'edit-owner', path, revision: ++serial, generation: serial, request: serial},
    position: source.length, source, schema: currentSchema};
}

function at(service, marked, path = 'main.ts', currentSchema = schemaText) {
  const position = marked.indexOf('¦');
  assert.notEqual(position, -1, 'completion fixture requires a caret');
  const source = marked.slice(0, position) + marked.slice(position + 1);
  const input = {...request(source, path, currentSchema), position};
  const result = service.complete(input);
  assert.deepEqual(result.key, input.key);
  return {source, result};
}

function accept(completion, label) {
  const candidate = completion.result.candidates.find(item => item.label === label);
  assert.ok(candidate, `Missing ${JSON.stringify(label)} in ${completion.result.candidates.map(item => item.label).join(', ')}`);
  assert.ok(candidate.from >= 0 && candidate.to >= candidate.from && candidate.to <= completion.source.length);
  return completion.source.slice(0, candidate.from) + candidate.insertText + completion.source.slice(candidate.to);
}

function compiler(input) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [compilerPath, '--transport-limit', String(8 * 1024 * 1024),
      '--deadline-ms', '15000', '--owner-pid', String(process.pid)], {stdio: ['pipe', 'pipe', 'pipe']});
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
    child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', code => {
      try {
        const response = JSON.parse(stdout);
        assert.equal(code, response.ok ? 0 : 1, stderr);
        resolve(response);
      } catch (error) { reject(error); }
    });
    // The ordinary driver uses an open control pipe as its owner-liveness contract.
    child.stdin.on('error', error => { if (error.code !== 'EPIPE') reject(error); });
    child.stdin.write(JSON.stringify(input) + '\n');
  });
}

async function compileSource(source, currentSchema = schema) {
  const input = {sources: {'main.ts': source}, schema: currentSchema, metadata: {}, resolutions: {}};
  const inspected = await compiler({...input, operation: 'inspect'});
  if (!inspected.ok) return inspected;
  return compiler({...input, operation: 'compile', compiler_identity: inspected.value.identity});
}

for (const path of ['main.ts', 'main.js']) {
  test(`${path}: SDK arguments, literals, results and nested options follow real TypeScript inference`, async t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const cases = [
      {scenario: 'method', marked: 'export const observation = host.call("ob¦serve", {});', label: 'observe',
        expected: 'export const observation = host.call("observe", {});'},
      {scenario: 'argument', marked: 'export const elapsed = host.call("wait", { dur¦ation_ms: 25 });', label: 'duration_ms',
        expected: 'export const elapsed = host.call("wait", { duration_ms: 25 });'},
      {scenario: 'literal', marked: 'export const recognized = host.call("recognize", {observation: host.call("observe", {}), roi: {x: 0, y: 0, width: 1, height: 1}, kind: "o¦cr"});', label: 'ocr',
        expected: 'export const recognized = host.call("recognize", {observation: host.call("observe", {}), roi: {x: 0, y: 0, width: 1, height: 1}, kind: "ocr"});'},
      {scenario: 'result', marked: 'export const frame = host.call("observe", {}).fra¦me;', label: 'frame',
        expected: 'export const frame = host.call("observe", {}).frame;'},
      {scenario: 'nested option', marked: 'export const threshold = host.options.settings.thres¦hold;', label: 'threshold',
        expected: 'export const threshold = host.options.settings.threshold;'},
      {scenario: 'option enum', marked: 'export const careful = host.options.settings.mode === "ca¦reful";', label: 'careful',
        expected: 'export const careful = host.options.settings.mode === "careful";'},
    ];
    const accepted = [];
    for (const row of cases) {
      await t.test(row.scenario, () => {
        const completion = at(service, row.marked, path);
        const source = accept(completion, row.label);
        assert.equal(source, row.expected);
        accepted.push(source);
      });
    }
    const source = accepted.join('\n');
    const compiled = await compileSource(source);
    assert.equal(compiled.ok, true, JSON.stringify(compiled));
    assert.equal(typeof compiled.value.sources['main.js'], 'string');
    if (path.endsWith('.js')) {
      const input = {sources: {[path]: source}, resolutions: {[path]: {}}};
      const inspected = await compiler({...input, operation: 'inspect-javascript'});
      assert.equal(inspected.ok, true, JSON.stringify(inspected));
      const linked = await compiler({...input, operation: 'link-javascript', compiler_identity: inspected.value.identity});
      assert.equal(linked.ok, true, JSON.stringify(linked));
    }
  });

  test(`${path}: invalid schema immediately withdraws old options and repair uses only current fields`, t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const original = at(service, 'host.options.¦', path);
    assert.equal(original.result.optionsAvailable, true);
    assert.ok(original.result.candidates.some(item => item.label === 'settings'));
    const changed = {version: 1, type: 'object', additionalProperties: false, required: [],
      properties: {renamed: {type: 'string', enum: ['new-value'], default: 'new-value'}}};
    const current = at(service, 'host.options.¦', path, JSON.stringify(changed));
    assert.deepEqual(current.result.candidates.map(item => item.label), ['renamed']);
    for (const invalid of [null, '{', JSON.stringify({...changed, additionalProperties: true}), JSON.stringify({...changed, version: 2})]) {
      const unavailable = at(service, 'host.options.¦', path, invalid);
      assert.equal(unavailable.result.optionsAvailable, false);
      assert.deepEqual(unavailable.result.candidates, []);
      const sdk = at(service, 'host.call("¦", {});', path, invalid);
      assert.equal(sdk.result.optionsAvailable, false);
      assert.ok(sdk.result.candidates.some(item => item.label === 'observe'));
    }
    const repaired = at(service, 'host.options.¦', path, JSON.stringify(changed));
    assert.equal(repaired.result.optionsAvailable, true);
    assert.deepEqual(repaired.result.candidates.map(item => item.label), ['renamed']);
    const discarded = at(service, 'host.options.settings.¦', path);
    assert.deepEqual(new Set(discarded.result.candidates.map(item => item.label)), new Set(['mode', 'threshold']));
  });

  test(`${path}: a local host shadows the runtime host instead of receiving injected SDK members`, t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const completion = at(service, 'export function run() { const host = {local: 42}; return host.¦; }', path);
    assert.deepEqual(completion.result.candidates.map(item => item.label), ['local']);
    assert.equal(accept(completion, 'local'), 'export function run() { const host = {local: 42}; return host.local; }');
  });

  test(`${path}: SDK method metadata follows resolved call provenance through inferred aliases`, async t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const direct = at(service, 'host.call("ob¦serve", {});', path);
    const observed = direct.result.candidates.find(item => item.label === 'observe');
    assert.ok(observed);
    const preview = ts.createSourceFile('preview.ts',
      observed.detail.replace(/^host\.call/, 'declare function preview') + ';', ts.ScriptTarget.ES2020, true);
    assert.deepEqual(preview.parseDiagnostics, []);
    const signature = preview.statements[0];
    assert.ok(ts.isFunctionDeclaration(signature));
    assert.equal(signature.parameters[0].type.literal.text, 'observe');
    assert.equal(signature.parameters[1].type.typeName.text, 'Record');
    assert.equal(signature.type.typeName.text, 'MadoObservation');
    assert.equal(typeof observed.documentation, 'string');
    assert.notEqual(observed.documentation, observed.label);
    assert.equal(observed.kind, ts.ScriptElementKind.string);
    assert.equal(accept(direct, 'observe'), 'host.call("observe", {});');
    for (const row of [
      {scenario: 'host object alias', marked: 'const runtime = host; runtime.call("ob¦serve", {});'},
      {scenario: 'function alias', marked: 'const invoke = host.call; invoke("ob¦serve", {});'},
      {scenario: 'destructured function alias', marked: 'const {call: invoke} = host; invoke("ob¦serve", {});'},
      {scenario: 'indexed function alias', marked: 'const invoke = host["call"]; invoke("ob¦serve", {});'},
    ]) {
      await t.test(row.scenario, () => {
        const completion = at(service, row.marked, path);
        const candidate = completion.result.candidates.find(item => item.label === 'observe');
        assert.ok(candidate);
        assert.equal(candidate.detail, observed.detail);
        assert.equal(candidate.documentation, observed.documentation);
        assert.equal(accept(completion, 'observe'), row.marked.replace('¦', ''));
      });
    }
  });
}

test('SDK metadata distinguishes argument and result contracts without weakening compiler admission', async t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const completion = at(service, 'host.call("¦", {});');
  const signatures = new Map();
  for (const candidate of completion.result.candidates) {
    const file = ts.createSourceFile('preview.ts',
      candidate.detail.replace(/^host\.call/, 'declare function preview') + ';', ts.ScriptTarget.ES2020, true);
    assert.deepEqual(file.parseDiagnostics, []);
    const signature = file.statements[0];
    assert.ok(ts.isFunctionDeclaration(signature));
    assert.equal(signature.parameters[0].type.literal.text, candidate.label);
    assert.equal(signature.parameters[1].name.text, 'args');
    assert.equal(accept(completion, candidate.label), `host.call("${candidate.label}", {});`);
    signatures.set(candidate.label, signature);
  }
  assert.deepEqual(signatures.get('submit').parameters[1].type.members.map(member => member.name.text),
    ['observation', 'actions']);
  assert.deepEqual(signatures.get('submit').type.members.map(member => member.name.text), ['id', 'order']);
  assert.equal(signatures.get('settle').type.typeName.text, 'MadoReceipt');
  assert.equal(signatures.get('target_start').type.typeName.text, 'MadoNativeProgress');
  assert.equal(signatures.get('target_status').type.typeName.text, 'MadoNativeProgress');
  const invalid = await compileSource('export const observation = host.call("observe", {override: true});');
  assert.equal(invalid.ok, false);
  assert.equal(invalid.fault.category, 'TypeScript');
});

for (const row of [
  {scenario: 'shadowed host with matching literal names',
    marked: 'export function run() { const host = {call(method: "observe" | "submit", args: unknown) {}}; return host.call("ob¦serve", {}); }'},
  {scenario: 'local generic using the SDK method type',
    marked: 'function invoke<K extends keyof MadoCalls>(method: K, args: MadoCalls[K]["args"]): MadoCalls[K]["result"] { throw new Error("inert"); } invoke("ob¦serve", {}); export {};'},
]) {
  test(`SDK metadata is not injected into ${row.scenario}`, t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const completion = at(service, row.marked);
    const candidate = completion.result.candidates.find(item => item.label === 'observe');
    assert.ok(candidate);
    assert.equal(candidate.documentation, undefined);
    assert.equal(candidate.detail?.startsWith('host.call(') ?? false, false);
    assert.equal(accept(completion, 'observe'), row.marked.replace('¦', ''));
  });
}

test('local and trusted-library signatures remain separate from plain-text documentation', t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const documentation = 'Normalize a selected value. <em>Plain text only.</em>';
  const local = at(service, `const local = {\n/** ${documentation} */\nmeasure(value: string): number { return value.length; }\n}; local.mea¦sure("x"); export {};`);
  const method = local.result.candidates.find(item => item.label === 'measure');
  assert.ok(method);
  assert.equal(method.documentation, documentation);
  assert.ok(method.detail.includes('value: string'));
  assert.ok(method.detail.includes(': number'));
  assert.equal(method.detail.includes(documentation), false);
  assert.ok(accept(local, 'measure').endsWith('local.measure("x"); export {};'));
  const library = at(service, 'export const result = ["x"].ma¦p(value => value.length);');
  const map = library.result.candidates.find(item => item.label === 'map');
  assert.ok(map);
  assert.ok(map.detail.includes('callbackfn:'));
  assert.equal(typeof map.documentation, 'string');
  assert.equal(map.detail.includes(map.documentation), false);
  assert.equal(accept(library, 'map'), 'export const result = ["x"].map(value => value.length);');
});

test('schema enum changes discard old literal alternatives and options stay readonly', async t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const marked = 'export const choice = host.options.settings.mode === "¦";';
  const original = at(service, marked);
  assert.deepEqual(new Set(original.result.candidates.map(item => item.label)), new Set(['fast', 'careful']));
  const changed = structuredClone(schema);
  changed.properties.settings.properties.mode.enum = ['repaired'];
  const current = at(service, marked, 'main.ts', JSON.stringify(changed));
  assert.deepEqual(current.result.candidates.map(item => item.label), ['repaired']);
  const readonly = await compileSource('host.options.settings.threshold = 1; export {};');
  assert.equal(readonly.ok, false);
  assert.equal(readonly.fault.category, 'TypeScript');
  assert.ok(readonly.fault.context.diagnostics.some(item => item.code === 2540));
});

const escapedName = '<tag>"\'\\\r\n${notCode}`😀';
const escapedSchema = {version: 1, type: 'object', additionalProperties: false, required: [escapedName],
  properties: {[escapedName]: {type: 'string'}}};
for (const row of [
  {scenario: 'dot becomes a bracket access', expression: 'host.options.¦'},
  {scenario: 'double-quoted bracket', expression: 'host.options["¦"]'},
  {scenario: 'single-quoted bracket', expression: "host.options['¦']"},
  {scenario: 'template-literal bracket', expression: 'host.options[`¦`]'},
]) {
  test(`escaped property insertion: ${row.scenario}`, async t => {
    const service = new ScriptLanguageService(libraries);
    t.after(() => service.dispose());
    const prefix = '// 日本語 😀\r\n';
    const completion = at(service, prefix + `export const selected = ${row.expression};\r\n`, 'main.ts', JSON.stringify(escapedSchema));
    const accepted = accept(completion, escapedName);
    assert.equal(accepted.slice(0, prefix.length), prefix);
    assert.equal(accepted.slice(-3), ';\r\n');
    const file = ts.createSourceFile('main.ts', accepted, ts.ScriptTarget.ES2020, true);
    const declaration = file.statements[0].declarationList.declarations[0];
    assert.ok(ts.isElementAccessExpression(declaration.initializer));
    assert.ok(ts.isStringLiteralLike(declaration.initializer.argumentExpression));
    assert.equal(declaration.initializer.argumentExpression.text, escapedName);
    const compiled = await compileSource(accepted, escapedSchema);
    assert.equal(compiled.ok, true, JSON.stringify(compiled));
  });
}

test('template enum insertion escapes interpolation instead of introducing executable expressions', async t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const value = '${missingExecutable()}\\`\r\n';
  const current = {version: 1, type: 'object', additionalProperties: false, required: ['mode'],
    properties: {mode: {type: 'string', enum: [value]}}};
  const completion = at(service, 'export const same = host.options.mode === `¦`;', 'main.ts', JSON.stringify(current));
  assert.equal(completion.result.candidates.length, 1);
  const candidate = completion.result.candidates[0];
  const accepted = accept(completion, candidate.label);
  const file = ts.createSourceFile('main.ts', accepted, ts.ScriptTarget.ES2020, true);
  const expression = file.statements[0].declarationList.declarations[0].initializer.right;
  assert.ok(ts.isNoSubstitutionTemplateLiteral(expression));
  assert.equal(expression.text, value);
  const compiled = await compileSource(accepted, current);
  assert.equal(compiled.ok, true, JSON.stringify(compiled));
});

test('UTF-16 replacement spans preserve CRLF, astral text and the suffix of a partially typed member', t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const prefix = '// 😀 日本語\r\nexport const observed = host.call("observe", {});\r\nobserved.';
  const completion = at(service, prefix + 'fra¦me;\r\n');
  const candidate = completion.result.candidates.find(item => item.label === 'frame');
  assert.ok(candidate);
  assert.equal(candidate.from, prefix.length);
  assert.equal(candidate.to, prefix.length + 'frame'.length);
  assert.equal(accept(completion, 'frame'), prefix + 'frame;\r\n');
});

test('analysis neither loads ambient libraries or external modules nor evaluates package effects', t => {
  const service = new ScriptLanguageService(new Map(Object.entries(libraries)));
  t.after(() => service.dispose());
  const calls = [];
  const original = {};
  for (const method of ['readFile', 'fileExists', 'readDirectory', 'directoryExists', 'getDirectories']) {
    original[method] = ts.sys[method];
    ts.sys[method] = (...args) => { calls.push([method, args]); throw new Error('External filesystem access'); };
  }
  const fetch = globalThis.fetch;
  globalThis.fetch = (...args) => { calls.push(['fetch', args]); throw new Error('External network access'); };
  t.after(() => { Object.assign(ts.sys, original); globalThis.fetch = fetch; });
  const source = '/// <reference lib="dom" />\n/// <reference types="node" />\n/// <reference path="/outside/helper.d.ts" />\n'
    + 'import {external} from "https://invalid.example/helper.js";\n'
    + 'globalThis.__madoCompletionMustNotExecute = true;\nthrow new Error("must remain inert");\n';
  assert.equal(globalThis.__madoCompletionMustNotExecute, undefined);
  const globals = at(service, source + 'globalThis.¦;');
  const names = new Set(globals.result.candidates.map(item => item.label));
  assert.ok(names.has('Promise'));
  for (const unavailable of ['window', 'document', 'fetch', 'process', 'Buffer', 'require', 'console']) {
    assert.equal(names.has(unavailable), false, unavailable);
  }
  const sdk = at(service, source + 'host.call("¦", {});');
  assert.ok(sdk.result.candidates.some(item => item.label === 'observe'));
  const imported = at(service, source + 'external.¦;');
  assert.deepEqual(imported.result.candidates, []);
  const es2020 = at(service, 'Promise.allSettled([]).¦;');
  assert.ok(es2020.result.candidates.some(item => item.label === 'then'));
  const es2021 = at(service, '"text".¦;');
  assert.equal(es2021.result.candidates.some(item => item.label === 'replaceAll'), false);
  assert.equal(globalThis.__madoCompletionMustNotExecute, undefined);
  assert.deepEqual(calls, []);
});

test('the injected trusted inventory refuses an additional ambient library and incomplete ES2020 closure', () => {
  assert.throws(() => new ScriptLanguageService({...libraries, 'lib.dom.d.ts': 'declare const window: any;'}));
  const incomplete = new Map(Object.entries(libraries));
  incomplete.delete('lib.es5.d.ts');
  assert.throws(() => new ScriptLanguageService(incomplete));
});

test('cached request fields retain only the current source/schema and cannot cross owner or missing-file context', t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const first = request('host.options.settings.');
  service.complete(first);
  const omitted = service.complete({kind: 'complete', key: {...first.key, request: ++serial}, position: first.position});
  assert.deepEqual(new Set(omitted.candidates.map(item => item.label)), new Set(['mode', 'threshold']));
  const changed = {...request('host.call("'), schema: null};
  const independent = service.complete(changed);
  assert.equal(independent.optionsAvailable, false);
  assert.ok(independent.candidates.some(item => item.label === 'observe'));
  const invalid = service.complete({kind: 'complete', key: {...changed.key, request: ++serial}, source: 'host.options.', position: 'host.options.'.length});
  assert.equal(invalid.optionsAvailable, false);
  assert.deepEqual(invalid.candidates, []);
  assert.throws(() => service.complete({kind: 'complete', key: {...first.key, owner: 'successor'}, position: 0}));
  assert.throws(() => service.complete({kind: 'complete', key: {...first.key, path: 'other.ts'}, position: 0}));
});

test('completion count and serialized response bounds disclose capping without truncating edits', t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const properties = Object.fromEntries(Array.from({length: 250}, (_, index) => [`field${index}`, {type: 'string'}]));
  const many = {version: 1, type: 'object', additionalProperties: false, required: [], properties};
  const count = at(service, 'host.options.¦', 'main.ts', JSON.stringify(many)).result;
  assert.equal(count.candidates.length, CANDIDATE_LIMIT);
  assert.equal(count.capped, true);
  for (const item of count.candidates) assert.ok(Object.hasOwn(properties, item.label));
  const largeProperties = Object.fromEntries(Array.from({length: 100}, (_, index) => ['x'.repeat(2500) + index, {type: 'string'}]));
  const large = at(service, 'host.options.¦', 'main.ts', JSON.stringify({...many, properties: largeProperties}));
  assert.equal(large.result.capped, true);
  assert.ok(large.result.candidates.length < CANDIDATE_LIMIT);
  assert.ok(Buffer.byteLength(JSON.stringify({kind: 'result', result: large.result})) <= RESPONSE_BYTES);
  for (const item of large.result.candidates) {
    assert.ok(Object.hasOwn(largeProperties, item.label));
    assert.equal(accept(large, item.label), 'host.options.' + item.label);
  }
  const bulkyKey = {...request('host.options.', 'main.ts', JSON.stringify(many)),
    key: {...request('').key, owner: 'owner'.repeat(50000)}};
  const bounded = service.complete(bulkyKey);
  assert.equal(bounded.capped, true);
  assert.ok(Buffer.byteLength(JSON.stringify({kind: 'result', result: bounded})) <= RESPONSE_BYTES);
});

test('the combined UTF-8 signature and documentation budget omits metadata without changing edits', t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const signatureSource = `measure(value: "${'x'.repeat(2200)}"): void {}`;
  const baseline = at(service, `const local = {${signatureSource}}; local.mea¦sure; export {};`);
  const original = baseline.result.candidates.find(item => item.label === 'measure');
  assert.ok(original);
  const signatureBytes = Buffer.byteLength(original.detail);
  assert.ok(signatureBytes < DETAIL_BYTES);
  const remaining = DETAIL_BYTES - signatureBytes;
  const documentation = '😀'.repeat(Math.floor(remaining / 4)) + 'x'.repeat(remaining % 4);
  const bounded = at(service, `const local = {\n/** ${documentation} */\n${signatureSource}\n}; local.mea¦sure; export {};`);
  const included = bounded.result.candidates.find(item => item.label === 'measure');
  assert.ok(included);
  assert.equal(Buffer.byteLength(included.detail) + Buffer.byteLength(included.documentation), DETAIL_BYTES);
  assert.equal(included.detailOmitted, undefined);
  const completion = at(service, `const local = {\n/** ${documentation}😀 */\n${signatureSource}\n}; local.mea¦sure; export {};`);
  const candidate = completion.result.candidates.find(item => item.label === 'measure');
  assert.ok(candidate);
  assert.ok(Buffer.byteLength(documentation + '😀') < DETAIL_BYTES);
  assert.equal(candidate.detailOmitted, true);
  assert.equal(candidate.detail, undefined);
  assert.equal(candidate.documentation, undefined);
  assert.equal(completion.result.capped, false);
  assert.equal(candidate.to - candidate.from, 'measure'.length);
  assert.ok(accept(completion, 'measure').endsWith('local.measure; export {};'));
});

test('input byte bounds, invalid positions and disposal fail without partial analysis results', () => {
  const service = new ScriptLanguageService(libraries);
  assert.throws(() => service.complete(request('😀'.repeat(AUTHORING_SOURCE_BYTES / 4 + 1))));
  assert.throws(() => service.complete({...request('host.'), position: -1}));
  assert.throws(() => service.complete({...request('host.'), position: 6}));
  assert.throws(() => service.complete(request('host.', 'main.ts', ' '.repeat(AUTHORING_SOURCE_BYTES + 1))));
  assert.throws(() => service.complete(request(' '.repeat(AUTHORING_SOURCE_BYTES / 2 + 1), 'main.ts',
    ' '.repeat(AUTHORING_SOURCE_BYTES / 2))));
  service.dispose();
  assert.throws(() => service.complete(request('host.')));
});

test('unknown editor options do not weaken strict compiler schema or import admission', async t => {
  const service = new ScriptLanguageService(libraries);
  t.after(() => service.dispose());
  const advisory = at(service, 'host.call("¦", {});', 'main.ts', 'null');
  assert.equal(advisory.result.optionsAvailable, false);
  assert.ok(advisory.result.candidates.some(item => item.label === 'observe'));
  assert.throws(() => definitions(null));
  const invalid = await compileSource('export const observed = host.call("observe", {});', null);
  assert.equal(invalid.ok, false);
  assert.equal(invalid.fault.category, 'Compiler');
  for (const row of [
    {scenario: 'erased external import', source: 'import type {Value} from "missing-package"; export type Alias = Value;', category: 'ImportRefused'},
    {scenario: 'relative helper', source: 'import {helper} from "./helper"; export {helper};', category: 'ImportRefused'},
    {scenario: 'ambient types', source: '/// <reference types="node" />\nexport {};', category: 'ImportRefused'},
    {scenario: 'DOM library directive', source: '/// <reference lib="dom" />\nexport {};', category: 'CompilerPolicy'},
  ]) {
    await t.test(row.scenario, async () => {
      const response = await compileSource(row.source);
      assert.equal(response.ok, false);
      assert.equal(response.fault.category, row.category);
    });
  }
});

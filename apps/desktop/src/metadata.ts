import {lineColumn} from './authoring.ts';
import type {FileDraft, TypedText} from './authoring.ts';
import {retainStoredText, storedText} from './recovery.ts';
import type {RecoveryEdit, StoredText} from './recovery.ts';
import {exactIntegerText, optionPath, readDraft} from './state.ts';
import type {AuthoringFileKind, Json, Schema} from './types.ts';

// Pure models behind Edit's structured views. Declared metadata (manifest, options schema, presets, source maps) is
// edited through forms that rewrite only the members the operator changes; every other member, including ones the
// forms do not understand, is carried over until a deliberate repair removes it.

export type JsonObject = {[key: string]: Json};

export function isObject(value: Json | undefined): value is JsonObject {
  return value !== null && value !== undefined && typeof value === 'object' && !Array.isArray(value);
}

// Own data property even for a `__proto__` key, exactly as JSON.parse would create it.
function define(target: JsonObject, key: string, value: Json) {
  Object.defineProperty(target, key, {value, enumerable: true, writable: true, configurable: true});
}

function without(source: JsonObject, key: string): JsonObject {
  const next: JsonObject = {};
  for (const [name, value] of Object.entries(source)) if (name !== key) define(next, name, value);
  return next;
}

// ---------------------------------------------------------------------------------------------------------------
// JSON documents

// serde_json's default recursion limit: the host refuses deeper documents too.
const JSON_DEPTH = 128;
export type JsonProblem = 'unexpectedCharacter' | 'unexpectedEnd' | 'invalidString' | 'invalidNumber' | 'trailingContent' | 'depth';
export interface JsonError {problem: JsonProblem; line: number; column: number}
// `numbers` are literals JavaScript cannot write back identically (1.0, 1e3, integers beyond 2^53) and `duplicates`
// are repeated member paths whose last value is the effective one, as in the host. Rewriting the document after an
// edit normalizes both, so the views disclose them before the first edit.
export type JsonDocument = {ok: true; value: Json; numbers: string[]; duplicates: string[]} | {ok: false; error: JsonError};

class JsonFailure extends Error {
  readonly problem: JsonProblem;
  readonly offset: number;
  constructor(problem: JsonProblem, offset: number) {
    super(problem);
    this.problem = problem;
    this.offset = offset;
  }
}

const ESCAPES: Record<string, string> = {'"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t'};
const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;

// Strict RFC 8259 parsing with a location for every refusal, independent of the WebView's JSON.parse messages.
export function parseJson(text: string): JsonDocument {
  let at = 0;
  const numbers: string[] = [];
  const duplicates: string[] = [];
  const fail = (problem: JsonProblem, offset = at): never => {throw new JsonFailure(at >= text.length && problem === 'unexpectedCharacter' ? 'unexpectedEnd' : problem, offset);};
  const space = () => {
    while (at < text.length && (text[at] === ' ' || text[at] === '\t' || text[at] === '\n' || text[at] === '\r')) at += 1;
  };
  const hex = (offset: number): number => {
    const digits = text.slice(offset, offset + 4);
    if (digits.length < 4) fail('unexpectedEnd', text.length);
    if (!/^[0-9A-Fa-f]{4}$/.test(digits)) fail('invalidString', offset);
    return Number.parseInt(digits, 16);
  };
  const string = (): string => {
    at += 1;
    let result = '';
    let start = at;
    for (;;) {
      if (at >= text.length) fail('unexpectedEnd');
      const code = text.charCodeAt(at);
      if (code === 0x22) {
        result += text.slice(start, at);
        at += 1;
        return result;
      }
      if (code < 0x20) fail('invalidString');
      if (code !== 0x5c) {
        at += 1;
        continue;
      }
      result += text.slice(start, at);
      const escape = text[at + 1];
      if (escape === undefined) fail('unexpectedEnd', at + 1);
      if (Object.hasOwn(ESCAPES, escape)) {
        result += ESCAPES[escape];
        at += 2;
      } else if (escape === 'u') {
        const unit = hex(at + 2);
        if (unit >= 0xdc00 && unit <= 0xdfff) fail('invalidString', at);
        if (unit >= 0xd800 && unit <= 0xdbff) {
          if (text[at + 6] !== '\\' || text[at + 7] !== 'u') fail('invalidString', at);
          const low = hex(at + 8);
          if (low < 0xdc00 || low > 0xdfff) fail('invalidString', at + 6);
          result += String.fromCharCode(unit, low);
          at += 12;
        } else {
          result += String.fromCharCode(unit);
          at += 6;
        }
      } else {
        fail('invalidString', at + 1);
      }
      start = at;
    }
  };
  const number = (): number => {
    NUMBER.lastIndex = at;
    const match = NUMBER.exec(text);
    if (!match) return fail('invalidNumber');
    const parsed = Number(match[0]);
    if (!Number.isFinite(parsed)) fail('invalidNumber');
    if (String(parsed) !== match[0]) numbers.push(match[0]);
    at += match[0].length;
    return parsed;
  };
  const value = (depth: number, path: string): Json => {
    space();
    if (at >= text.length) fail('unexpectedEnd');
    const character = text[at];
    if (character === '{' || character === '[') {
      if (depth >= JSON_DEPTH) fail('depth');
      at += 1;
      space();
      const close = character === '{' ? '}' : ']';
      const object: JsonObject = {};
      const array: Json[] = [];
      if (text[at] === close) {
        at += 1;
        return character === '{' ? object : array;
      }
      for (;;) {
        if (character === '{') {
          space();
          if (text[at] !== '"') fail('unexpectedCharacter');
          const key = string();
          space();
          if (text[at] !== ':') fail('unexpectedCharacter');
          at += 1;
          const member = optionPath(path, key);
          const item = value(depth + 1, member);
          if (Object.hasOwn(object, key)) duplicates.push(member);
          define(object, key, item);
        } else {
          array.push(value(depth + 1, `${path}[${array.length}]`));
        }
        space();
        if (text[at] === ',') {
          at += 1;
        } else if (text[at] === close) {
          at += 1;
          return character === '{' ? object : array;
        } else {
          fail('unexpectedCharacter');
        }
      }
    }
    if (character === '"') return string();
    if (character === '-' || (character >= '0' && character <= '9')) return number();
    for (const [word, literal] of [['true', true], ['false', false], ['null', null]] as const) {
      if (text.startsWith(word, at)) {
        at += word.length;
        return literal;
      }
    }
    return fail('unexpectedCharacter');
  };
  try {
    const result = value(0, '$');
    space();
    if (at < text.length) fail('trailingContent');
    return {ok: true, value: result, numbers, duplicates};
  } catch (error) {
    if (!(error instanceof JsonFailure)) throw error;
    const {line, column} = lineColumn(text, Math.min(error.offset, text.length));
    return {ok: false, error: {problem: error.problem, line, column}};
  }
}

// Structural equality; member order is not part of a JSON object's meaning.
export function jsonEqual(left: Json | undefined, right: Json | undefined): boolean {
  if (left === right) return true;
  if (Array.isArray(left)) return Array.isArray(right) && left.length === right.length && left.every((item, index) => jsonEqual(item, right[index]));
  if (!isObject(left) || !isObject(right)) return false;
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every(key => Object.hasOwn(right, key) && jsonEqual(left[key], right[key]));
}

// The document text for `value`. A value equal to the saved document is the saved text byte for byte, so restoring
// every field makes the file clean again and untouched files never change; otherwise the saved text's indentation
// (or compactness) and final newline are kept.
export function formatJson(saved: string | null, value: Json): string {
  if (saved !== null) {
    const parsed = parseJson(saved);
    if (parsed.ok && jsonEqual(parsed.value, value)) return saved;
  }
  const indent = saved === null ? '  ' : /\n([ \t]+)\S/.exec(saved)?.[1] ?? '';
  const newline = saved === null || saved.endsWith('\n') ? '\n' : '';
  return JSON.stringify(value, null, indent) + newline;
}

// ---------------------------------------------------------------------------------------------------------------
// Files tree

export interface TreeFolder {kind: 'folder'; name: string; path: string; children: TreeNode[]}
export interface TreeFile {kind: 'file'; name: string; draft: FileDraft}
export type TreeNode = TreeFolder | TreeFile;

// Program files and assets form the folder tree; declarations and generated data have their own structured views.
export function treeKind(kind: AuthoringFileKind): boolean {
  return kind === 'source' || kind === 'asset';
}

function compareNames(left: string, right: string): number {
  const a = left.toLowerCase();
  const b = right.toLowerCase();
  if (a !== b) return a < b ? -1 : 1;
  return left < right ? -1 : left > right ? 1 : 0;
}

function sortFolder(folder: TreeFolder) {
  folder.children.sort((left, right) => left.kind !== right.kind ? (left.kind === 'folder' ? -1 : 1) : compareNames(left.name, right.name));
  for (const child of folder.children) if (child.kind === 'folder') sortFolder(child);
}

// Folders exist only as the parents of declared files: there is no empty-folder state to create, keep or remove,
// matching the package snapshot, where an empty directory is not package content.
export function fileTree(drafts: readonly FileDraft[]): TreeNode[] {
  const root: TreeFolder = {kind: 'folder', name: '', path: '', children: []};
  for (const draft of drafts) {
    if (!treeKind(draft.kind)) continue;
    const parts = draft.path.split('/');
    let folder = root;
    for (let index = 0; index < parts.length - 1; index++) {
      const name = parts[index];
      let next = folder.children.find((node): node is TreeFolder => node.kind === 'folder' && node.name === name);
      if (!next) {
        next = {kind: 'folder', name, path: parts.slice(0, index + 1).join('/'), children: []};
        folder.children.push(next);
      }
      folder = next;
    }
    folder.children.push({kind: 'file', name: parts[parts.length - 1], draft});
  }
  sortFolder(root);
  return root.children;
}

export function folderAncestors(path: string): string[] {
  const parts = path.split('/');
  return parts.slice(0, -1).map((_, index) => parts.slice(0, index + 1).join('/'));
}

// ---------------------------------------------------------------------------------------------------------------
// Manifest

export const HELPER_DEPENDENCY = '@mado/helper';
export const HELPER_VERSION = '1.0.0';
// Desktop authoring opens JavaScript and TypeScript packages only.
export const AUTHORING_RUNTIMES: readonly string[] = ['typescript', 'javascript'];
export type EntryName = 'readiness' | 'workflow';
export const ENTRY_NAMES: readonly EntryName[] = ['readiness', 'workflow'];

export interface ManifestTarget {id: string; windowTitle: string | null; bundleId: string | null}
export interface ManifestAsset {id: string; path: string; format: string; width: Json | undefined; height: Json | undefined}
export interface ManifestView {
  packageId: string; version: Json | undefined; runtime: string; sdk: string; entryContract: string;
  entries: Record<EntryName, {module: string; function: string}>;
  sources: string[]; schema: string; profiles: [string, string][]; assets: ManifestAsset[]; sourceMaps: [string, string][];
  helper: boolean; target: ManifestTarget | null;
}

function text(value: Json | undefined): string {
  return typeof value === 'string' ? value : '';
}

function members(value: Json | undefined): [string, Json][] {
  return isObject(value) ? Object.entries(value) : [];
}

export function readManifest(value: Json): ManifestView | null {
  if (!isObject(value)) return null;
  const entries: JsonObject = isObject(value.entries) ? value.entries : {};
  const entry = (name: EntryName) => {
    const item = entries[name];
    return isObject(item) ? {module: text(item.module), function: text(item.function)} : {module: '', function: ''};
  };
  const target = value.target;
  const macos = isObject(target) ? target.macos : undefined;
  return {
    packageId: text(value.package_id), version: value.version, runtime: text(value.runtime), sdk: text(value.sdk), entryContract: text(value.entry_contract),
    entries: {readiness: entry('readiness'), workflow: entry('workflow')},
    sources: Array.isArray(value.sources) ? value.sources.filter((item): item is string => typeof item === 'string') : [],
    schema: text(value.schema),
    profiles: members(value.profiles).map(([id, path]): [string, string] => [id, text(path)]),
    assets: members(value.assets).map(([id, asset]) => isObject(asset)
      ? {id, path: text(asset.path), format: text(asset.format), width: asset.width, height: asset.height}
      : {id, path: '', format: '', width: undefined, height: undefined}),
    sourceMaps: members(value.source_maps).map(([module, path]): [string, string] => [module, text(path)]),
    helper: isObject(value.dependencies) && value.dependencies[HELPER_DEPENDENCY] === HELPER_VERSION,
    target: isObject(target) ? {id: text(target.id), windowTitle: typeof target.window_title === 'string' ? target.window_title : null,
      bundleId: isObject(macos) && typeof macos.bundle_id === 'string' ? macos.bundle_id : null} : null,
  };
}

export function withEntry(manifest: JsonObject, name: EntryName, field: 'module' | 'function', value: string): JsonObject {
  const entries: JsonObject = isObject(manifest.entries) ? manifest.entries : {};
  const entry = entries[name];
  return {...manifest, entries: {...entries, [name]: {...(isObject(entry) ? entry : {}), [field]: value}}};
}

export function withHelper(manifest: JsonObject, enabled: boolean): JsonObject {
  const dependencies: JsonObject = isObject(manifest.dependencies) ? without(manifest.dependencies, HELPER_DEPENDENCY) : {};
  if (enabled) define(dependencies, HELPER_DEPENDENCY, HELPER_VERSION);
  return {...manifest, dependencies};
}

// The declaration is portable intent only. An empty window title or macOS bundle ID means "not declared": the host
// refuses empty values there, so the form never has to write one.
export function withTarget(manifest: JsonObject, target: ManifestTarget | null): JsonObject {
  const rest = without(manifest, 'target');
  if (target === null) return rest;
  const previous: JsonObject = isObject(manifest.target) ? manifest.target : {};
  const next: JsonObject = {...without(previous, 'macos'), id: target.id, window_title: target.windowTitle};
  if (target.bundleId !== null) next.macos = {...(isObject(previous.macos) ? previous.macos : {}), bundle_id: target.bundleId};
  return {...rest, target: next};
}

export function entryFunctionValid(value: string): boolean {
  return value.length <= 128 && /^[A-Za-z_][A-Za-z0-9_]*$/.test(value);
}

export function windowTitleValid(value: string): boolean {
  return new TextEncoder().encode(value).length <= 512 && !/\p{Cc}/u.test(value);
}

export function bundleIdValid(value: string): boolean {
  return /^[A-Za-z0-9.-]{1,255}$/.test(value);
}

// ---------------------------------------------------------------------------------------------------------------
// Options schema: the host's deliberately small keyword set

export const SCHEMA_TYPES = ['object', 'array', 'string', 'number', 'integer', 'boolean'] as const;
export type SchemaType = typeof SCHEMA_TYPES[number];
// The host refuses nesting deeper than this.
export const SCHEMA_DEPTH = 32;
const SPECIFIC: Record<SchemaType, readonly string[]> = {
  object: ['properties', 'required', 'additionalProperties'], array: ['items', 'minItems', 'maxItems'],
  string: ['enum', 'minLength', 'maxLength'], number: ['minimum', 'maximum'], integer: ['minimum', 'maximum'], boolean: [],
};
// [lower, upper, whole non-negative numbers only]
export const SCHEMA_BOUNDS: Partial<Record<SchemaType, readonly [string, string, boolean]>> = {
  array: ['minItems', 'maxItems', true], string: ['minLength', 'maxLength', true],
  number: ['minimum', 'maximum', false], integer: ['minimum', 'maximum', false],
};
const KEYWORDS: Record<string, true> = Object.fromEntries(['type', 'default', 'version', ...Object.values(SPECIFIC).flat()].map(key => [key, true]));

export type SchemaIssue =
  | {code: 'node'} | {code: 'type'} | {code: 'version'} | {code: 'rootType'} | {code: 'keyword'; key: string}
  | {code: 'additionalProperties'} | {code: 'properties'} | {code: 'required'} | {code: 'items'}
  | {code: 'bound'; key: string} | {code: 'reversed'} | {code: 'enum'} | {code: 'depth'};
export interface LocatedIssue {path: string; issue: SchemaIssue}

export function schemaType(node: Json | undefined): SchemaType | null {
  const type = isObject(node) ? node.type : undefined;
  return typeof type === 'string' && (SCHEMA_TYPES as readonly string[]).includes(type) ? type as SchemaType : null;
}

export function schemaProperties(node: JsonObject): [string, Json][] {
  return members(node.properties);
}

export function requiredNames(node: JsonObject): string[] {
  return Array.isArray(node.required) ? node.required.filter((item): item is string => typeof item === 'string') : [];
}

export function boundValid(value: Json | undefined, whole: boolean): boolean {
  return typeof value === 'number' && Number.isFinite(value) && (!whole || (Number.isInteger(value) && value >= 0));
}

const NUMBER_TEXT = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/;

// A typed constraint: no member when empty, the number when it is exactly what was typed, and otherwise the typed
// text, which stays visibly invalid. An integer beyond double precision, an overflow or negative zero is therefore
// never rounded silently into the document.
export function constraintValue(text: string): Json | undefined {
  const trimmed = text.trim();
  if (trimmed === '') return undefined;
  const number = Number(trimmed);
  if (!NUMBER_TEXT.test(trimmed) || !Number.isFinite(number) || Object.is(number, -0)) return text;
  return Number.isInteger(number) && !exactIntegerText(trimmed, number) ? text : number;
}

function validRequired(node: JsonObject): string[] {
  const properties = isObject(node.properties) ? node.properties : {};
  return [...new Set(requiredNames(node).filter(name => Object.hasOwn(properties, name)))];
}

function validEnum(values: Json | undefined): string[] {
  return Array.isArray(values) ? [...new Set(values.filter((item): item is string => typeof item === 'string'))] : [];
}

// Problems of one schema node itself (children are separate nodes), in the host's validation terms.
export function nodeIssues(node: Json | undefined, root = false): SchemaIssue[] {
  if (!isObject(node)) return [{code: 'node'}];
  const issues: SchemaIssue[] = [];
  if (root && node.version !== 1) issues.push({code: 'version'});
  const type = schemaType(node);
  if (root && type !== 'object') return [...issues, {code: 'rootType'}];
  if (type === null) return [...issues, {code: 'type'}];
  for (const key of Object.keys(node)) {
    if (key !== 'type' && key !== 'default' && !(root && key === 'version') && !SPECIFIC[type].includes(key)) issues.push({code: 'keyword', key});
  }
  if (type === 'object') {
    if (node.additionalProperties !== false) issues.push({code: 'additionalProperties'});
    if (!isObject(node.properties)) issues.push({code: 'properties'});
    const required = node.required;
    if (!Array.isArray(required) || required.length !== validRequired(node).length || required.some(item => typeof item !== 'string')) issues.push({code: 'required'});
  }
  if (type === 'array' && node.items === undefined) issues.push({code: 'items'});
  const bounds = SCHEMA_BOUNDS[type];
  if (bounds) {
    const [lower, upper, whole] = bounds;
    for (const key of [lower, upper]) if (node[key] !== undefined && !boundValid(node[key], whole)) issues.push({code: 'bound', key});
    const low = node[lower];
    const high = node[upper];
    if (boundValid(low, whole) && boundValid(high, whole) && (low as number) > (high as number)) issues.push({code: 'reversed'});
  }
  if (type === 'string' && node.enum !== undefined && (!Array.isArray(node.enum) || node.enum.length === 0 || validEnum(node.enum).length !== node.enum.length)) {
    issues.push({code: 'enum'});
  }
  return issues;
}

export function schemaIssues(root: Json): LocatedIssue[] {
  const found: LocatedIssue[] = [];
  const walk = (node: Json | undefined, path: string, depth: number) => {
    if (depth > SCHEMA_DEPTH) {
      found.push({path, issue: {code: 'depth'}});
      return;
    }
    for (const issue of nodeIssues(node, depth === 0)) found.push({path, issue});
    if (!isObject(node)) return;
    const type = schemaType(node);
    if (type === 'object') for (const [name, child] of schemaProperties(node)) walk(child, optionPath(path, name), depth + 1);
    if (type === 'array' && node.items !== undefined) walk(node.items, `${path}[]`, depth + 1);
  };
  walk(root, '$', 0);
  return found;
}

export function repairable(issue: SchemaIssue): boolean {
  return issue.code !== 'type' && issue.code !== 'reversed' && issue.code !== 'depth';
}

// One deliberate repair: it changes only what the issue names and keeps every other member.
export function repairNode(node: Json | undefined, issue: SchemaIssue): Json {
  if (!isObject(node)) return {type: 'string'};
  switch (issue.code) {
    case 'version': return {...node, version: 1};
    case 'rootType': return withType(node, 'object');
    case 'keyword': case 'bound': return without(node, issue.key);
    case 'additionalProperties': return {...node, additionalProperties: false};
    case 'properties': return {...node, properties: {}};
    case 'required': return {...node, required: validRequired(node)};
    case 'items': return {...node, items: {type: 'string'}};
    case 'enum': {
      const values = validEnum(node.enum);
      return values.length > 0 ? {...node, enum: values} : without(node, 'enum');
    }
    default: return node;
  }
}

// A type change drops the previous type's constraints and its default, which described values of that type; members
// the host does not support stay (still reported) until deliberately removed.
export function withType(node: Json | undefined, type: SchemaType): JsonObject {
  const source: JsonObject = isObject(node) ? node : {};
  if (schemaType(source) === type) return source;
  const next: JsonObject = {type};
  for (const [key, value] of Object.entries(source)) {
    if (key !== 'type' && key !== 'default' && (key === 'version' || !Object.hasOwn(KEYWORDS, key) || SPECIFIC[type].includes(key))) define(next, key, value);
  }
  if (type === 'object') {
    if (!isObject(next.properties)) next.properties = {};
    if (!Array.isArray(next.required)) next.required = [];
    next.additionalProperties = false;
  }
  if (type === 'array' && next.items === undefined) next.items = {type: 'string'};
  return next;
}

export function withKeyword(node: JsonObject, key: string, value: Json | undefined): JsonObject {
  return value === undefined ? without(node, key) : {...node, [key]: value};
}

// Replacing an existing field keeps its position; a new field is appended.
export function withProperty(node: JsonObject, name: string, child: Json): JsonObject {
  return {...node, properties: {...(isObject(node.properties) ? node.properties : {}), [name]: child}};
}

export function withoutProperty(node: JsonObject, name: string): JsonObject {
  const next: JsonObject = {...node, properties: without(isObject(node.properties) ? node.properties : {}, name)};
  if (Array.isArray(node.required)) next.required = node.required.filter(item => item !== name);
  return next;
}

export function renameProperty(node: JsonObject, from: string, to: string): JsonObject {
  const properties: JsonObject = {};
  for (const [name, child] of schemaProperties(node)) define(properties, name === from ? to : name, child);
  const next: JsonObject = {...node, properties};
  if (Array.isArray(node.required)) next.required = node.required.map(item => item === from ? to : item);
  return next;
}

export function withRequired(node: JsonObject, name: string, required: boolean): JsonObject {
  const names = Array.isArray(node.required) ? node.required.filter(item => item !== name) : [];
  return {...node, required: required ? [...names, name] : names};
}

export function emptySchema(): JsonObject {
  return {version: 1, type: 'object', additionalProperties: false, required: [], properties: {}};
}

// Only top-level defaults fill omitted preset options; nested defaults are validated but never applied.
export function topDefaults(root: JsonObject): Record<string, Json> {
  const defaults: Record<string, Json> = {};
  for (const [name, child] of schemaProperties(root)) if (isObject(child) && child.default !== undefined) define(defaults, name, child.default);
  return defaults;
}

export function withTopDefaults(root: JsonObject, defaults: Record<string, Json>): JsonObject {
  const properties: JsonObject = {};
  for (const [name, child] of schemaProperties(root)) {
    define(properties, name, !isObject(child) ? child : Object.hasOwn(defaults, name) ? {...child, default: defaults[name]} : without(child, 'default'));
  }
  return {...root, properties};
}

// ---------------------------------------------------------------------------------------------------------------
// Option values edited through SchemaForm (top-level defaults and preset options)

// SchemaForm keeps numeric text as strings until it parses. `committed` is the value written to the document:
// numbers once the text parses, the operator's text while it does not. `stored` names strings that were already
// document data under a numeric field; they stay type mismatches until deliberately replaced, never converted.
export interface ValuesState {schema: Schema; draft: Record<string, Json>; stored: StoredText[]; committed: Record<string, Json>}

// `typed` is the provenance recorded with the file draft: strings the operator typed are editable text again when the
// form is shown anew, as long as the document still holds exactly that text there.
export function loadValues(schema: Schema, value: Record<string, Json>, typed: readonly TypedText[] = []): ValuesState {
  const stored = storedText(schema, value).filter(entry => !typed.some(item => item.path === entry.path && item.text === entry.text));
  return {schema, draft: value, stored, committed: value};
}

// The provenance to record with the document: operator text under numeric fields that is not a number yet.
export function typedText(state: ValuesState): TypedText[] {
  const stored = new Set(state.stored.map(entry => entry.path));
  return storedText(state.schema, state.committed).filter(entry => !stored.has(entry.path)).map(({path, text}) => ({path, text}));
}

function withValueAt(root: Json, segments: readonly (string | number)[], leaf: Json): Json {
  if (segments.length === 0) return leaf;
  const [head, ...rest] = segments;
  if (typeof head === 'number') return Array.isArray(root) ? root.map((item, index) => index === head ? withValueAt(item, rest, leaf) : item) : root;
  return isObject(root) && Object.hasOwn(root, head) ? {...root, [head]: withValueAt(root[head], rest, leaf)} : root;
}

// The state for the document's current value under the current schema. A value changed elsewhere (Refresh, a repair),
// or provenance the draft no longer records (Discard), is read again from the document and the recorded provenance.
// A schema change keeps the value but re-derives provenance: text the operator is typing into a field that stays
// numeric remains editable text, and every other string under a numeric field is stored data.
export function currentValues(state: ValuesState, schema: Schema, value: Record<string, Json>, typed: readonly TypedText[] = []): ValuesState {
  const recorded = typedText(state).every(entry => typed.some(item => item.path === entry.path && item.text === entry.text));
  if (!recorded || !jsonEqual(state.committed, value)) return loadValues(schema, value, typed);
  if (state.schema === schema) return state;
  const stored = new Set(state.stored.map(entry => entry.path));
  const editing = new Set(storedText(state.schema, state.draft).filter(entry => !stored.has(entry.path)).map(entry => entry.path));
  let draft: Json = value;
  for (const entry of storedText(schema, state.draft)) if (editing.has(entry.path)) draft = withValueAt(draft, entry.segments, entry.text);
  return {schema, draft: draft as Record<string, Json>, stored: storedText(schema, value).filter(entry => !editing.has(entry.path)), committed: value};
}

// One SchemaForm edit: it retires only its own stored mismatches, and the committed value converts only operator text.
export function editValues(state: ValuesState, draft: Record<string, Json>, edit: RecoveryEdit): ValuesState {
  const stored = retainStoredText(state.stored, draft, edit);
  return {schema: state.schema, draft, stored, committed: readDraft(state.schema, draft, 'en', new Set(stored.map(entry => entry.path))).values};
}

// ---------------------------------------------------------------------------------------------------------------
// Packaged presets

export type PresetIssue = {code: 'root'} | {code: 'packageId'} | {code: 'schemaVersion'} | {code: 'options'} | {code: 'member'; key: string};
const PRESET_MEMBERS = ['package_id', 'schema_version', 'options'];

export function presetIssues(value: Json, packageId: string, schemaVersion: Json | undefined): PresetIssue[] {
  if (!isObject(value)) return [{code: 'root'}];
  const issues: PresetIssue[] = [];
  if (value.package_id !== packageId) issues.push({code: 'packageId'});
  // An unreadable schema has no version to compare against; the member itself is still required.
  if (value.schema_version === undefined || (schemaVersion !== undefined && !jsonEqual(value.schema_version, schemaVersion))) issues.push({code: 'schemaVersion'});
  if (!isObject(value.options)) issues.push({code: 'options'});
  for (const key of Object.keys(value)) if (!PRESET_MEMBERS.includes(key)) issues.push({code: 'member', key});
  return issues;
}

export function starterPreset(packageId: string, schemaVersion: Json | undefined): JsonObject {
  return {package_id: packageId, schema_version: schemaVersion ?? 1, options: {}};
}

export function repairPreset(value: Json, issue: PresetIssue, packageId: string, schemaVersion: Json | undefined): JsonObject {
  if (!isObject(value) || issue.code === 'root') return starterPreset(packageId, schemaVersion);
  switch (issue.code) {
    case 'packageId': return {...value, package_id: packageId};
    case 'schemaVersion': return {...value, schema_version: schemaVersion ?? 1};
    case 'options': return {...value, options: {}};
    case 'member': return without(value, issue.key);
  }
}

// ---------------------------------------------------------------------------------------------------------------
// Source maps: generated data, shown as facts only

export type SourceMapIssue = {code: 'root'} | {code: 'version'} | {code: 'sources'} | {code: 'mappings'} | {code: 'sourceRoot'} | {code: 'source'; index: number};
export interface SourceMapFacts {
  version: Json | undefined; file: string | null; sources: string[]; names: number | null; mappings: number | null;
  embedded: number; issues: SourceMapIssue[];
}

export function sourceMapFacts(value: Json): SourceMapFacts {
  if (!isObject(value)) return {version: undefined, file: null, sources: [], names: null, mappings: null, embedded: 0, issues: [{code: 'root'}]};
  const issues: SourceMapIssue[] = [];
  if (value.version !== 3) issues.push({code: 'version'});
  const sources = Array.isArray(value.sources) ? value.sources : null;
  if (sources === null) issues.push({code: 'sources'});
  sources?.forEach((item, index) => {if (typeof item !== 'string') issues.push({code: 'source', index});});
  if (typeof value.mappings !== 'string') issues.push({code: 'mappings'});
  if (value.sourceRoot !== undefined && value.sourceRoot !== '') issues.push({code: 'sourceRoot'});
  return {
    version: value.version, file: typeof value.file === 'string' ? value.file : null,
    sources: (sources ?? []).filter((item): item is string => typeof item === 'string'),
    names: Array.isArray(value.names) ? value.names.length : null,
    mappings: typeof value.mappings === 'string' ? value.mappings.length : null,
    embedded: Array.isArray(value.sourcesContent) ? value.sourcesContent.filter(item => typeof item === 'string').length : 0,
    issues,
  };
}

import {AUTHORING_RUNTIMES, SCHEMA_BOUNDS, SCHEMA_DEPTH, SCHEMA_TYPES, formatJson, isObject, nodeIssues, parseJson, presetIssues, readManifest,
  renameProperty, repairNode, repairPreset, repairable, schemaIssues, schemaProperties, schemaType, topDefaults, withEntry, withHelper, withKeyword,
  withProperty, withRequired, withTarget, withTopDefaults, withType, withoutProperty} from './metadata.ts';
import type {EntryName, JsonObject, ManifestTarget, PresetIssue, SchemaIssue, SchemaType} from './metadata.ts';
import {MAX_DEFINITIONS, MAX_DOCUMENT_BYTES, MAX_EXPECTED_BYTES, MAX_NAME_BYTES, createDefinition, deleteDefinition, regionFromEdges, renameDefinition,
  sameJson, setContent, setExpected, setKind, setRegion, setRights, utf8Bytes} from './recognition.ts';
import type {Edges, PixelRect, RecognitionKind, RecognitionNotice, RecognitionState, TemplateRights} from './recognition.ts';
import type {Json} from './types.ts';

// Typed structured draft operations for collaboration edits. Each operation is one control of an existing structured
// view (the manifest, options schema and preset forms, the recognition definition list and preview) applied through
// that view's own helper, so untouched members, malformed parts and recorded numeric text stay as the person left
// them. Nothing here writes arbitrary JSON paths or host-owned saved-asset references.

// Property names select object members and `null` selects an array's item schema, from the root (`[]`).
export type SchemaNode = (string|null)[];
export type FieldOperation =
  // Manifest form.
  | {field:'runtime'; value:string}
  | {field:'entry'; entry:EntryName; part:'module'|'function'; value:string}
  | {field:'helper'; enabled:boolean}
  | {field:'target'; target:ManifestTarget|null}
  // Options schema form.
  | {field:'type'; node:SchemaNode; type:SchemaType}
  | {field:'bound'; node:SchemaNode; key:string; value:number|null}
  | {field:'enum'; node:SchemaNode; values:string[]}
  | {field:'addProperty'; node:SchemaNode; name:string; type:SchemaType}
  | {field:'removeProperty'; node:SchemaNode; name:string}
  | {field:'renameProperty'; node:SchemaNode; name:string; to:string}
  | {field:'required'; node:SchemaNode; name:string; required:boolean}
  | {field:'removeDefault'; node:SchemaNode}
  | {field:'default'; name:string; value:Json|null}
  | {field:'repair'; node:SchemaNode; issue:SchemaIssue}
  // Packaged preset form.
  | {field:'option'; name:string; value:Json|null}
  | {field:'repair'; issue:PresetIssue}
  // Recognition definition, content basis and capture document.
  | {field:'name'; value:string}
  | {field:'expected'; value:string|null}
  | {field:'kind'; value:RecognitionKind}
  | {field:'region'; edges:Edges}
  | {field:'search'; edges:Edges}
  | {field:'delete'}
  | {field:'content'; content:PixelRect}
  | {field:'create'; name:string; edges:Edges}
  | {field:'rights'; rights:TemplateRights|null};
export type FieldDomain = 'manifest'|'schema'|'preset'|'definition'|'basis'|'context';
export interface PatchFailure {code:string; message:string}
export type RecognitionTarget = {role:'definition'; id:string} | {role:'basis'} | {role:'context'};
export interface MetadataScope {packageId:string; schema:string|null}

const VALUE_DEPTH = 64;

export function record(value:unknown):Record<string,unknown>|null {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string,unknown> : null;
}

export function only(value:Record<string,unknown>, keys:readonly string[]):boolean {
  return Object.keys(value).every(key => keys.includes(key));
}

function exact(value:Record<string,unknown>, keys:readonly string[]):boolean {
  return only(value, keys) && keys.every(key => Object.hasOwn(value, key));
}

// Valid Unicode only: a lone surrogate would corrupt the draft and cannot cross the JSON boundary again.
export function wellFormed(text:string):boolean {
  for (let index = 0; index < text.length; index++) {
    const unit = text.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff && index + 1 < text.length) {
      const next = text.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        index++;
        continue;
      }
    }
    if (unit >= 0xd800 && unit <= 0xdfff) return false;
  }
  return true;
}

function text(value:unknown):value is string {
  return typeof value === 'string' && wellFormed(value);
}

function integer(value:unknown):value is number {
  return typeof value === 'number' && Number.isSafeInteger(value);
}

function jsonValue(value:unknown, depth = 0):value is Json {
  if (value === null || typeof value === 'boolean') return true;
  if (typeof value === 'number') return Number.isFinite(value);
  if (typeof value === 'string') return wellFormed(value);
  if (depth >= VALUE_DEPTH) return false;
  if (Array.isArray(value)) return value.every(item => jsonValue(item, depth + 1));
  const object = record(value);
  return object !== null && Object.entries(object).every(([key, item]) => wellFormed(key) && jsonValue(item, depth + 1));
}

function schemaNode(value:unknown):SchemaNode|null {
  return Array.isArray(value) && value.length <= SCHEMA_DEPTH + 1 && value.every(segment => segment === null || text(segment)) ? [...value] : null;
}

function edges(value:unknown):Edges|null {
  const item = record(value);
  return item && exact(item, ['left', 'top', 'right', 'bottom']) && integer(item.left) && integer(item.top) && integer(item.right) && integer(item.bottom)
    ? {left: item.left, top: item.top, right: item.right, bottom: item.bottom} : null;
}

function issue(value:unknown):{code:string; key?:string}|null {
  const item = record(value);
  if (!item || !only(item, ['code', 'key']) || typeof item.code !== 'string' || (item.key !== undefined && !text(item.key))) return null;
  return item.key === undefined ? {code: item.code} : {code: item.code, key: item.key};
}

// The shape of one requested field operation; whether it applies to the addressed resource is checked later.
export function parseField(value:unknown):FieldOperation|null {
  const op = record(value);
  if (!op) return null;
  switch (op.field) {
    case 'runtime':
      return exact(op, ['field', 'value']) && text(op.value) ? {field: 'runtime', value: op.value} : null;
    case 'name':
      return exact(op, ['field', 'value']) && text(op.value) ? {field: 'name', value: op.value} : null;
    case 'entry':
      return exact(op, ['field', 'entry', 'part', 'value']) && (op.entry === 'readiness' || op.entry === 'workflow')
        && (op.part === 'module' || op.part === 'function') && text(op.value)
        ? {field: 'entry', entry: op.entry as EntryName, part: op.part as 'module'|'function', value: op.value} : null;
    case 'helper':
      return exact(op, ['field', 'enabled']) && typeof op.enabled === 'boolean' ? {field: 'helper', enabled: op.enabled} : null;
    case 'target': {
      if (!exact(op, ['field', 'target'])) return null;
      if (op.target === null) return {field: 'target', target: null};
      const target = record(op.target);
      if (!target || !exact(target, ['id', 'windowTitle', 'bundleId']) || !text(target.id) || !(target.windowTitle === null || text(target.windowTitle))
        || !(target.bundleId === null || text(target.bundleId))) return null;
      // As in the form, an empty title or bundle ID means "not declared".
      return {field: 'target', target: {id: target.id, windowTitle: (target.windowTitle as string|null) || null, bundleId: (target.bundleId as string|null) || null}};
    }
    case 'type': case 'addProperty': {
      const node = schemaNode(op.node);
      const type = typeof op.type === 'string' && (SCHEMA_TYPES as readonly string[]).includes(op.type) ? op.type as SchemaType : null;
      if (op.field === 'type') return exact(op, ['field', 'node', 'type']) && node && type ? {field: 'type', node, type} : null;
      return exact(op, ['field', 'node', 'name', 'type']) && node && type && text(op.name) ? {field: 'addProperty', node, name: op.name, type} : null;
    }
    case 'bound': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node', 'key', 'value']) && node && text(op.key) && (op.value === null || (typeof op.value === 'number' && Number.isFinite(op.value)))
        ? {field: 'bound', node, key: op.key, value: op.value} : null;
    }
    case 'enum': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node', 'values']) && node && Array.isArray(op.values) && op.values.every(text)
        ? {field: 'enum', node, values: [...op.values]} : null;
    }
    case 'removeProperty': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node', 'name']) && node && text(op.name) ? {field: 'removeProperty', node, name: op.name} : null;
    }
    case 'renameProperty': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node', 'name', 'to']) && node && text(op.name) && text(op.to) ? {field: 'renameProperty', node, name: op.name, to: op.to} : null;
    }
    case 'required': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node', 'name', 'required']) && node && text(op.name) && typeof op.required === 'boolean'
        ? {field: 'required', node, name: op.name, required: op.required} : null;
    }
    case 'removeDefault': {
      const node = schemaNode(op.node);
      return exact(op, ['field', 'node']) && node ? {field: 'removeDefault', node} : null;
    }
    case 'default': case 'option':
      return exact(op, ['field', 'name', 'value']) && text(op.name) && jsonValue(op.value)
        ? {field: op.field as 'default'|'option', name: op.name, value: op.value} : null;
    case 'repair': {
      const found = issue(op.issue);
      if (!found) return null;
      // Only issues the view currently reports are ever applied, so the shape alone is checked here.
      if (Object.hasOwn(op, 'node')) {
        const node = schemaNode(op.node);
        return exact(op, ['field', 'node', 'issue']) && node ? {field: 'repair', node, issue: found as SchemaIssue} : null;
      }
      return exact(op, ['field', 'issue']) ? {field: 'repair', issue: found as PresetIssue} : null;
    }
    case 'expected':
      return exact(op, ['field', 'value']) && (op.value === null || text(op.value)) ? {field: 'expected', value: op.value} : null;
    case 'kind':
      return exact(op, ['field', 'value']) && (op.value === 'ocr' || op.value === 'template') ? {field: 'kind', value: op.value as RecognitionKind} : null;
    case 'region': case 'search': {
      const box = edges(op.edges);
      return exact(op, ['field', 'edges']) && box ? {field: op.field as 'region'|'search', edges: box} : null;
    }
    case 'delete':
      return exact(op, ['field']) ? {field: 'delete'} : null;
    case 'content': {
      const content = record(op.content);
      return exact(op, ['field', 'content']) && content && exact(content, ['x', 'y', 'width', 'height']) && integer(content.x) && integer(content.y)
        && integer(content.width) && integer(content.height) ? {field: 'content', content: {x: content.x, y: content.y, width: content.width, height: content.height}} : null;
    }
    case 'create': {
      const box = edges(op.edges);
      return exact(op, ['field', 'name', 'edges']) && text(op.name) && box ? {field: 'create', name: op.name, edges: box} : null;
    }
    case 'rights': {
      if (!exact(op, ['field', 'rights'])) return null;
      if (op.rights === null) return {field: 'rights', rights: null};
      const rights = record(op.rights);
      return rights && exact(rights, ['license', 'created_by', 'created_for', 'reviewed']) && text(rights.license) && text(rights.created_by)
        && (rights.created_for === null || text(rights.created_for)) && typeof rights.reviewed === 'boolean'
        ? {field: 'rights', rights: {license: rights.license, created_by: rights.created_by, created_for: rights.created_for as string|null, reviewed: rights.reviewed}}
        : null;
    }
    default:
      return null;
  }
}

export function fieldDomain(op:FieldOperation):FieldDomain {
  switch (op.field) {
    case 'runtime': case 'entry': case 'helper': case 'target': return 'manifest';
    case 'option': return 'preset';
    case 'repair': return 'node' in op ? 'schema' : 'preset';
    case 'name': case 'expected': case 'kind': case 'region': case 'search': case 'delete': return 'definition';
    case 'content': return 'basis';
    case 'create': case 'rights': return 'context';
    default: return 'schema';
  }
}

type Step = {ok:true; value:Json} | {ok:false; failure:PatchFailure};

function done(value:Json):Step {
  return {ok: true, value};
}

function fail(code:string, message:string):Step {
  return {ok: false, failure: {code, message}};
}

function manifestField(value:Json, op:FieldOperation):Step {
  const manifest = readManifest(value);
  if (!isObject(value) || manifest === null) return fail('malformed_document', 'The manifest is not a JSON object; repair it in the desktop');
  switch (op.field) {
    case 'runtime':
      return AUTHORING_RUNTIMES.includes(op.value) ? done({...value, runtime: op.value})
        : fail('invalid_field', `The runtime must be one of ${AUTHORING_RUNTIMES.join(', ')}`);
    case 'entry':
      if (op.part === 'module' && !manifest.sources.some(path => path === op.value && !path.endsWith('.d.ts'))) {
        return fail('invalid_field', 'An entry module must be a declared source module');
      }
      return done(withEntry(value, op.entry, op.part, op.value));
    case 'helper':
      return done(withHelper(value, op.enabled));
    case 'target':
      return done(withTarget(value, op.target));
    default:
      return fail('unsupported_field', 'This field does not apply to the manifest');
  }
}

// The node at `path` and how to put a replacement back, exactly as the nested schema form passes its changes up.
function locate(root:JsonObject, path:SchemaNode):{node:Json; rebuild:(next:Json) => Json}|null {
  let node:Json = root;
  const steps:((child:Json) => Json)[] = [];
  for (const segment of path) {
    if (!isObject(node)) return null;
    const parent:JsonObject = node;
    if (segment === null) {
      if (schemaType(parent) !== 'array' || parent.items === undefined) return null;
      steps.push(child => ({...parent, items: child}));
      node = parent.items;
    } else {
      const properties = parent.properties;
      if (schemaType(parent) !== 'object' || !isObject(properties) || !Object.hasOwn(properties, segment)) return null;
      steps.push(child => withProperty(parent, segment, child));
      node = properties[segment];
    }
  }
  return {node, rebuild: next => steps.reduceRight((child, step) => step(child), next)};
}

function propertyNames(node:JsonObject):string[] {
  return schemaProperties(node).map(([name]) => name);
}

function schemaNodeField(node:Json, path:SchemaNode, op:FieldOperation):Step {
  const object = isObject(node) ? node : null;
  const type = schemaType(node);
  // The node whose fields the form lists: an object schema with a properties object.
  const fields = object !== null && type === 'object' && isObject(object.properties) ? object : null;
  switch (op.field) {
    case 'type':
      return path.length > 0 ? done(withType(node, op.type)) : fail('invalid_field', 'The root type is changed only by its repair');
    case 'bound': {
      const bounds = type === null ? undefined : SCHEMA_BOUNDS[type];
      if (!object || !bounds || (op.key !== bounds[0] && op.key !== bounds[1])) return fail('invalid_field', 'This node has no such bound');
      return done(withKeyword(object, op.key, op.value ?? undefined));
    }
    case 'enum':
      if (!object || path.length === 0 || type !== 'string' || (object.enum !== undefined && !Array.isArray(object.enum))) {
        return fail('invalid_field', 'Only string fields with a list enum accept enum values');
      }
      return done(withKeyword(object, 'enum', op.values.length === 0 ? undefined : op.values));
    case 'addProperty':
      if (fields === null || path.length >= SCHEMA_DEPTH) return fail('invalid_field', 'This node cannot hold another field');
      if (op.name === '' || propertyNames(fields).includes(op.name)) return fail('invalid_field', 'A new field needs an unused, non-empty name');
      return done(withProperty(fields, op.name, withType(undefined, op.type)));
    case 'removeProperty': case 'renameProperty': case 'required': {
      if (fields === null || !propertyNames(fields).includes(op.name)) return fail('invalid_field', 'The field does not exist');
      if (op.field === 'removeProperty') return done(withoutProperty(fields, op.name));
      if (op.field === 'required') return done(withRequired(fields, op.name, op.required));
      if (op.to === op.name) return done(fields);
      if (op.to === '' || propertyNames(fields).includes(op.to)) return fail('invalid_field', 'A field needs an unused, non-empty name');
      return done(renameProperty(fields, op.name, op.to));
    }
    case 'removeDefault':
      // Top-level defaults belong to the defaults form; nested defaults are only ever removed.
      if (path.length === 0 || (path.length === 1 && path[0] !== null)) return fail('invalid_field', 'Top-level defaults are changed with the default field');
      if (!object || object.default === undefined) return fail('invalid_field', 'This node has no default');
      return done(withKeyword(object, 'default', undefined));
    case 'repair': {
      if (!('node' in op)) return fail('unsupported_field', 'This field does not apply to the options schema');
      const root = path.length === 0;
      const found = nodeIssues(node, root).find(item => sameJson(item, op.issue));
      if (!found || !repairable(found) || (root && found.code === 'node')) return fail('invalid_field', 'That issue is not reported here or is not repairable');
      return done(repairNode(node, found));
    }
    default:
      return fail('unsupported_field', 'This field does not apply to the options schema');
  }
}

function schemaField(root:Json, op:FieldOperation):Step {
  // A malformed root offers only its own repairs, as in the form.
  if (op.field === 'repair' && 'node' in op && op.node.length === 0) return schemaNodeField(root, [], op);
  if (!isObject(root) || schemaType(root) !== 'object') return fail('malformed_document', 'The schema root is not an object schema; repair it first');
  if (op.field === 'default') {
    if (schemaIssues(root).length > 0) return fail('invalid_field', 'Defaults can be edited once the schema has no issues');
    if (!propertyNames(root).includes(op.name)) return fail('invalid_field', 'Defaults apply to declared top-level fields');
    return done(withTopDefaults(root, withKeyword(topDefaults(root), op.name, op.value ?? undefined)));
  }
  if (!('node' in op)) return fail('unsupported_field', 'This field does not apply to the options schema');
  const found = locate(root, op.node);
  if (!found) return fail('invalid_field', 'The schema node does not exist');
  const next = schemaNodeField(found.node, op.node, op);
  return next.ok ? done(found.rebuild(next.value)) : next;
}

function presetField(value:Json, op:FieldOperation, scope:MetadataScope):Step {
  if (!isObject(value)) return fail('malformed_document', 'The preset is not a JSON object; repair it in the desktop');
  const schemaDocument = scope.schema === null ? null : parseJson(scope.schema);
  const schemaRoot = schemaDocument?.ok ? schemaDocument.value : undefined;
  const schemaVersion = isObject(schemaRoot) ? schemaRoot.version : undefined;
  if (op.field === 'repair' && !('node' in op)) {
    const found = presetIssues(value, scope.packageId, schemaVersion).find(item => sameJson(item, op.issue));
    return found ? done(repairPreset(value, found, scope.packageId, schemaVersion)) : fail('invalid_field', 'That issue is not reported for this preset');
  }
  if (op.field !== 'option') return fail('unsupported_field', 'This field does not apply to a preset');
  // The options form follows the current options schema draft and exists only while that schema has no issues.
  if (!isObject(schemaRoot) || schemaIssues(schemaRoot).length > 0) return fail('invalid_field', 'Preset options wait for a valid options schema');
  const options = value.options;
  if (!isObject(options)) return fail('malformed_document', 'The preset has no options object; repair it first');
  if (!propertyNames(schemaRoot).includes(op.name)) return fail('invalid_field', 'The option is not declared by the options schema');
  return done({...value, options: withKeyword(options, op.name, op.value ?? undefined)});
}

// The document text after applying `fields` in order to one metadata draft, rewritten like a form edit (the saved
// text's layout, and the saved text itself when every value is restored). Malformed documents are never rewritten.
export function patchMetadata(kind:'manifest'|'schema'|'profile', current:string, base:string|null, fields:readonly FieldOperation[],
  scope:MetadataScope):string|PatchFailure {
  const document = parseJson(current);
  if (!document.ok) {
    return {code: 'malformed_document', message: `The document is not JSON (line ${document.error.line}, column ${document.error.column}); repair it in the desktop`};
  }
  let value = document.value;
  for (const op of fields) {
    const step = kind === 'manifest' ? manifestField(value, op) : kind === 'schema' ? schemaField(value, op) : presetField(value, op, scope);
    if (!step.ok) return step.failure;
    value = step.value;
  }
  return formatJson(base, value);
}

const NOTICE_FAILURES:Record<RecognitionNotice, PatchFailure> = {
  definitionLimit: {code: 'recognition_limit', message: `Captures would exceed ${MAX_DEFINITIONS} definitions`},
  documentLimit: {code: 'recognition_limit', message: `Recognition metadata would exceed ${MAX_DOCUMENT_BYTES} bytes`},
  expectedLimit: {code: 'invalid_field', message: `Reference text is limited to ${MAX_EXPECTED_BYTES} UTF-8 bytes`},
  nameLimit: {code: 'invalid_field', message: `Names are limited to ${MAX_NAME_BYTES} UTF-8 bytes without NUL`},
  invalidGeometry: {code: 'invalid_geometry', message: 'The geometry must be a non-empty rectangle inside the frame and content area'},
  staleEdit: {code: 'stale_version', message: 'The recognition draft changed; read it again'},
  confirmBeforeReplace: {code: 'ineligible', message: 'Confirm the current geometry in the desktop first'},
};

// Applies `fields` in order to the active capture's draft with the existing recognition helpers, so normalization,
// limits, freshness marks and bounded Undo behave as for the person's own edits. Each operation is one Undo step
// that never coalesces with adjacent human edits; selection, preview display and messages stay the person's.
export function patchRecognition(state:RecognitionState, target:RecognitionTarget, fields:readonly FieldOperation[]):{state:RecognitionState; created:string[]}|PatchFailure {
  let current = state;
  const created:string[] = [];
  for (const op of fields) {
    const document = current.document;
    if (document === null) return {code: 'ineligible', message: 'No recognition document is loaded in the desktop'};
    const definition = target.role === 'definition' ? document.definitions.find(item => item.id === target.id) : undefined;
    if (target.role === 'definition' && !definition) return {code: 'unknown_resource', message: 'The definition no longer exists'};
    const before:RecognitionState = {...current, group: null, notice: null};
    let next:RecognitionState;
    switch (op.field) {
      case 'name': next = renameDefinition(before, definition!.id, op.value); break;
      case 'expected': next = setExpected(before, definition!.id, op.value ?? ''); break;
      case 'kind': next = setKind(before, definition!.id, op.value); break;
      case 'region': case 'search': {
        if (op.field === 'search' && definition!.template === null) return {code: 'invalid_field', message: 'Only template definitions have a search region'};
        const region = regionFromEdges(op.edges, document.basis);
        if (region === null) return NOTICE_FAILURES.invalidGeometry;
        next = setRegion(before, definition!.id, op.field, region);
        break;
      }
      case 'delete': next = deleteDefinition(before, definition!.id); break;
      case 'content': next = setContent(before, op.content); break;
      case 'create': {
        if (utf8Bytes(op.name) > MAX_NAME_BYTES || op.name.includes('\0')) return NOTICE_FAILURES.nameLimit;
        const region = regionFromEdges(op.edges, document.basis);
        if (region === null) return NOTICE_FAILURES.invalidGeometry;
        next = createDefinition(before, region, op.name);
        const added = next.document?.definitions.find(item => !document.definitions.some(old => old.id === item.id));
        if (added) created.push(added.id);
        break;
      }
      case 'rights': next = setRights(before, op.rights); break;
      default: return {code: 'unsupported_field', message: 'This field does not apply to recognition metadata'};
    }
    if (next.notice !== null) return NOTICE_FAILURES[next.notice];
    // The person's selection survives agent edits unless its definition was deleted.
    const person = state.selected;
    const selected = person !== null && next.document?.definitions.some(item => item.id === person) ? person : next.selected;
    current = next.document === document ? current : {...next, selected, display: state.display};
  }
  if (current === state) return {state, created};
  return {state: {...current, group: null, notice: state.notice, error: state.error}, created};
}

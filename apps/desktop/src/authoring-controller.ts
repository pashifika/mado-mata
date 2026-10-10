import {sdk} from '../../../tools/runtime-comparison/compiler/sdk.mjs';
import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES, applyExternalEdits, diagnosticLocation, draftList, fileDirty, otherNonImageBytes, sameAuthoringRef,
  utf8Bytes} from './authoring.ts';
import type {AuthoringSession, TypedText, ValidationState} from './authoring.ts';
import {captureDocuments, geometryConfirmed, mapRegion, recognitionDirty, sameJson} from './recognition.ts';
import type {CaptureDocument, RecognitionDefinition, RecognitionDocument, RecognitionState} from './recognition.ts';
import {fieldDomain, only, parseField, patchMetadata, patchRecognition, record, wellFormed} from './structured-patch.ts';
import type {FieldDomain, FieldOperation, RecognitionTarget} from './structured-patch.ts';
import type {Json} from './types.ts';
import type {HostResult} from './authoring-requests.ts';
import {sourceDifference} from './editor/source-positions.ts';

// Collaboration wire contract. The owner is the host-issued Edit lease identity, which the host also checks when a
// request is claimed. Package, resource, version and cursor identities are opaque outside this controller and never
// carry host paths or credentials; the claimed request was already authenticated by the host.
export const COLLABORATION_PROTOCOL = 1;
// The compiler SDK contract package sources are written against.
export const COLLABORATION_SDK = sdk.contract;
export const PAGE_UNITS = 65_536;
export const EDIT_TARGETS = 64;
export const EDIT_RANGES = 4_096;
export const EDIT_FIELDS = 256;
export const EDIT_DEPENDENCIES = 64;
// A notice page coalesces at most this many resources; the history keeps the latest change of this many resources.
export const NOTICE_LIMIT = 256;
export const NOTICE_HISTORY = 1_024;
// The host's printable-ASCII bound for owner, package, resource, version and cursor identities.
const IDENTITY = /^[!-~]{1,256}$/;
const PACKAGE_IDENTITIES = 256;

export interface CollaborationLimits {
  pageUnits:number; sourceBytes:number; nonImageBytes:number; editTargets:number; editRanges:number; editFields:number; dependencies:number; notices:number;
}
const LIMITS:CollaborationLimits = {pageUnits: PAGE_UNITS, sourceBytes: AUTHORING_SOURCE_BYTES, nonImageBytes: AUTHORING_NON_IMAGE_BYTES,
  editTargets: EDIT_TARGETS, editRanges: EDIT_RANGES, editFields: EDIT_FIELDS, dependencies: EDIT_DEPENDENCIES, notices: NOTICE_LIMIT};

// Recognition resources: `context` is the savable aggregate of every capture's draft; the others are read-only
// provenance for Save and individually versioned snippet inputs.
export type CollaborationRole = 'context'|'definition'|'basis'|'template';
export interface CollaborationResourceInfo {path:string; kind:string; version:string; capture?:string; role?:CollaborationRole}
export interface CollaborationResource extends CollaborationResourceInfo {id:string; dirty:boolean; composing:boolean; missing:boolean}
export interface CollaborationRecognition {capture:string|null; frame:{width:number; height:number}|null; confirmed:boolean; maxOcrZones:number|null}
// The saved revision a validation checked; `excluded` names the unsaved resources it could not include.
export interface CollaborationValidation {revision:string; valid:boolean; current:boolean; diagnostics:number; excluded:string[]}
export interface CollaborationDescription {
  protocol:number; sdk:string; owner:string|null; package:string|null; packageId:string|null; savedRevision:string|null;
  selected:string|null; resources:CollaborationResource[]; pending:string|null; conflict:boolean; refreshRequired:boolean;
  limits:CollaborationLimits; cursor:string|null; validation:CollaborationValidation|null; recognition:CollaborationRecognition|null;
}
export interface CollaborationPage {
  resource:string; path:string; version:string; text:string; offset:number; nextOffset:number|null; totalUnits:number; savedRevision:string;
  // Incomplete numeric form input recorded with a metadata draft; part of its version.
  typed?:TypedText[];
}
export interface CollaborationDependency {resource:string; version:string}
// `version` is null for a target the edit removed (a deleted recognition definition).
export interface CollaborationEditResult {
  resources:{resource:string; version:string|null}[]; created:{resource:string; path:string; version:string}[]; savedRevision:string;
}
// The current state of one resource changed after the cursor; `version` null means it no longer exists.
export interface CollaborationNotice {resource:string; path:string; kind:string; version:string|null; capture?:string; role?:CollaborationRole}
// `gap` requires describe/read before relying on changes: `start` without a cursor, `expired` for a cursor of
// lost history or an earlier session, `owner` when the request names another (or no) Edit owner.
export interface CollaborationNotices {
  cursor:string|null; gap:'start'|'expired'|'owner'|null; changes:CollaborationNotice[]; more:boolean; savedRevision:string|null;
}
export interface CollaborationError {code:string; message:string; resource?:string; outcome?:'not_applied'|'unknown'}
// Closed, JSON-safe results from the draft controller and main-window host coordinator.
export type CollaborationResult = CollaborationDescription|CollaborationPage|CollaborationEditResult|CollaborationNotices|HostResult;
export type CollaborationResponse =
  | {owner:string|null; ok:true; result:CollaborationResult}
  | {owner:string|null; ok:false; error:CollaborationError};
// Why the main window currently refuses agent draft edits (for example a lost lease), or null when it admits them.
// `recognition` is true when the batch changes recognition metadata.
export type CollaborationEditBlock = (session:AuthoringSession, recognition:boolean) => string|null;

interface RangeEdit {from:number; to:number; text:string}
type TargetEdit = {resource:string; version:string; text:string} | {resource:string; version:string; ranges:RangeEdit[]}
  | {resource:string; version:string; fields:FieldOperation[]};
type Operation = {kind:'describe'}
  | {kind:'read'; resource:string; version:string|null; offset:number; limit:number}
  | {kind:'edit'; edits:TargetEdit[]; dependencies:CollaborationDependency[]}
  | {kind:'notices'; cursor:string|null; limit:number};
interface Request {owner:string|null; package:string|null; operation:Operation}

// One logical resource of a session lineage: a declared file, the recognition aggregate, a capture's basis or saved
// template provenance, a definition, or the latest validation result.
type Target = {type:'file'; path:string} | {type:'context'} | {type:'basis'; capture:string} | {type:'template'; capture:string}
  | {type:'definition'; capture:string; definition:string} | {type:'validation'};
// `version` names one uninterrupted state; `signature` is the content it was issued for (logical resources only).
interface Entry {
  id:string; version:string; target:Target; present:boolean; signature:string|null; path:string; kind:string; capture?:string; role?:CollaborationRole;
}
// The identities of one AuthoringSession lineage. A new host owner, or closing and recreating the session, starts a new
// lineage, so no resource, version or cursor identity is ever accepted for a later session. `changes` holds the
// sequence number of each resource's latest change in ascending order; `floor` is the newest evicted one.
interface Lineage {
  serial:number; owner:string; package:string; packagePath:string; entries:Map<string,Entry>; ids:Map<string,Entry>;
  seq:number; floor:number; changes:Map<string,number>;
  recognition:RecognitionState|null; validation:ValidationState|null; validationPath:string;
}
interface Splice {from:number; to:number; length:number}
interface Content {path:string; text:string|null; typed?:TypedText[]}

// The field domain a resource accepts, by file kind or recognition target type.
const FIELD_DOMAINS:Partial<Record<string, FieldDomain>> = {manifest: 'manifest', schema: 'schema', profile: 'preset',
  definition: 'definition', basis: 'basis', context: 'context'};

function refusal(code:string, message:string, resource?:string):CollaborationError {
  return resource === undefined ? {code, message, outcome: 'not_applied'} : {code, message, resource, outcome: 'not_applied'};
}

function invalid(message:string):CollaborationError {
  return refusal('invalid_request', message);
}

function identity(value:unknown):value is string {
  return typeof value === 'string' && IDENTITY.test(value);
}

function units(value:unknown):value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function parseRanges(value:unknown, budget:{ranges:number}):RangeEdit[]|null {
  if (!Array.isArray(value) || value.length === 0 || value.length > budget.ranges) return null;
  budget.ranges -= value.length;
  const ranges:RangeEdit[] = [];
  for (const item of value) {
    const range = record(item);
    if (!range || !only(range, ['from', 'to', 'text']) || !units(range.from) || !units(range.to) || typeof range.text !== 'string'
      || !wellFormed(range.text)) return null;
    ranges.push({from: range.from, to: range.to, text: range.text});
  }
  return ranges;
}

function parseFields(value:unknown, budget:{fields:number}):FieldOperation[]|null {
  if (!Array.isArray(value) || value.length === 0 || value.length > budget.fields) return null;
  budget.fields -= value.length;
  const fields:FieldOperation[] = [];
  for (const item of value) {
    const field = parseField(item);
    if (field === null) return null;
    fields.push(field);
  }
  return fields;
}

function parseOperation(value:unknown):Operation|CollaborationError {
  const operation = record(value);
  if (!operation) return invalid('The operation must be an object');
  switch (operation.kind) {
    case 'describe':
      return only(operation, ['kind']) ? {kind: 'describe'} : invalid('Describe takes no arguments');
    case 'read': {
      const {resource, version, offset, limit} = operation;
      if (!only(operation, ['kind', 'resource', 'version', 'offset', 'limit']) || !identity(resource)
        || (version !== undefined && !identity(version)) || (offset !== undefined && !units(offset))
        || (limit !== undefined && (!units(limit) || limit < 1 || limit > PAGE_UNITS))) return invalid('Malformed read request');
      return {kind: 'read', resource, version: version ?? null, offset: offset ?? 0, limit: limit ?? PAGE_UNITS};
    }
    case 'notices': {
      const {cursor, limit} = operation;
      if (!only(operation, ['kind', 'cursor', 'limit']) || (cursor !== undefined && !identity(cursor))
        || (limit !== undefined && (!units(limit) || limit < 1 || limit > NOTICE_LIMIT))) return invalid('Malformed notices request');
      return {kind: 'notices', cursor: cursor ?? null, limit: limit ?? NOTICE_LIMIT};
    }
    case 'edit': {
      const {edits, dependencies} = operation;
      if (!only(operation, ['kind', 'edits', 'dependencies']) || !Array.isArray(edits) || edits.length === 0 || edits.length > EDIT_TARGETS
        || (dependencies !== undefined && (!Array.isArray(dependencies) || dependencies.length > EDIT_DEPENDENCIES))) {
        return invalid('Malformed edit request');
      }
      const budget = {ranges: EDIT_RANGES, fields: EDIT_FIELDS};
      const targets:TargetEdit[] = [];
      const seen = new Set<string>();
      for (const item of edits) {
        const edit = record(item);
        if (!edit || !only(edit, ['resource', 'version', 'text', 'ranges', 'fields']) || !identity(edit.resource) || !identity(edit.version)
          || ['text', 'ranges', 'fields'].filter(name => name in edit).length !== 1) {
          return invalid('Each edit names one resource, its version and either text, ranges or fields');
        }
        if (seen.has(edit.resource)) return invalid('A resource can be edited once per request');
        seen.add(edit.resource);
        if ('text' in edit) {
          if (typeof edit.text !== 'string' || !wellFormed(edit.text)) return invalid('Replacement text must be a valid Unicode string');
          targets.push({resource: edit.resource, version: edit.version, text: edit.text});
        } else if ('ranges' in edit) {
          const ranges = parseRanges(edit.ranges, budget);
          if (ranges === null) return invalid(`Ranges must be 1-${EDIT_RANGES} {from,to,text} entries of valid Unicode text in total`);
          targets.push({resource: edit.resource, version: edit.version, ranges});
        } else {
          const fields = parseFields(edit.fields, budget);
          if (fields === null) return invalid(`Fields must be 1-${EDIT_FIELDS} supported field operations in total`);
          targets.push({resource: edit.resource, version: edit.version, fields});
        }
      }
      const reads:CollaborationDependency[] = [];
      for (const item of dependencies ?? []) {
        const dependency = record(item);
        if (!dependency || !only(dependency, ['resource', 'version']) || !identity(dependency.resource) || !identity(dependency.version)) {
          return invalid('Each dependency names one resource and its version');
        }
        reads.push({resource: dependency.resource, version: dependency.version});
      }
      return {kind: 'edit', edits: targets, dependencies: reads};
    }
    default:
      return refusal('unsupported_operation', 'The desktop does not support this operation');
  }
}

function parseRequest(value:unknown):Request|CollaborationError {
  const request = record(value);
  if (!request) return invalid('The request must be an object');
  const {owner, package: packageId} = request;
  if ((owner !== null && !identity(owner)) || (packageId !== null && !identity(packageId))) return invalid('Malformed owner or package identity');
  const operation = parseOperation(request.operation);
  return 'code' in operation ? operation : {owner, package: packageId, operation};
}

function high(unit:number):boolean {
  return unit >= 0xd800 && unit <= 0xdbff;
}

function low(unit:number):boolean {
  return unit >= 0xdc00 && unit <= 0xdfff;
}

// True when an offset falls between the two UTF-16 units of one non-BMP character.
function splits(text:string, offset:number):boolean {
  return offset > 0 && offset < text.length && high(text.charCodeAt(offset - 1)) && low(text.charCodeAt(offset));
}

// Maps an original offset through ascending, non-overlapping splices. A position at an insertion point or inside a
// replaced span stays before the new text, so the person's caret is never pushed by an agent insertion.
function mapPosition(position:number, splices:readonly Splice[]):number {
  let shift = 0;
  for (const splice of splices) {
    if (position <= splice.from) break;
    if (position < splice.to) return splice.from + shift;
    shift += splice.length - (splice.to - splice.from);
  }
  return position + shift;
}

// Applies UTF-16 ranges against the original text. Ranges may arrive in any order but must not overlap; insertions at
// the same offset keep their request order. Boundaries never split a non-BMP character, and nothing is normalized.
function spliceRanges(original:string, ranges:readonly RangeEdit[]):{text:string; splices:Splice[]}|string {
  const ordered = ranges.map((range, index) => ({range, index}))
    .sort((left, right) => left.range.from - right.range.from || left.range.to - right.range.to || left.index - right.index)
    .map(item => item.range);
  const parts:string[] = [];
  const splices:Splice[] = [];
  let cursor = 0;
  for (const range of ordered) {
    if (range.from > range.to || range.to > original.length) return 'A range lies outside the current text';
    if (range.from < cursor) return 'Ranges overlap';
    if (splits(original, range.from) || splits(original, range.to)) return 'A range boundary splits a character';
    parts.push(original.slice(cursor, range.from), range.text);
    splices.push({from: range.from, to: range.to, length: range.text.length});
    cursor = range.to;
  }
  parts.push(original.slice(cursor));
  return {text: parts.join(''), splices};
}

function key(target:Target):string {
  switch (target.type) {
    case 'file': return `f:${target.path}`;
    case 'definition': return JSON.stringify([target.type, target.capture, target.definition]);
    case 'basis': case 'template': return JSON.stringify([target.type, target.capture]);
    default: return target.type;
  }
}

// Every capture document of the session: the active capture's local draft and the host's documents of the others.
// A loaded frame without a host document yet has only the active local draft.
function captureList(state:RecognitionState):CaptureDocument[] {
  const listed = captureDocuments(state);
  const active = state.view.capture_id;
  return active !== null && state.document !== null && !listed.some(capture => capture.capture_id === active)
    ? [...listed, {capture_id: active, document: state.document}] : listed;
}

function savedDocument(state:RecognitionState, capture:string):RecognitionDocument|null {
  return state.view.saved_captures.find(item => item.capture_id === capture)?.document ?? null;
}

// What generated source and the definition list show of one definition. The saved diagnostic crop is host-owned and
// listed only in the aggregate; the fence revision is bookkeeping.
function definitionContent(definition:RecognitionDefinition) {
  const {id, name, kind, region, expected, template} = definition;
  return {id, name, kind, region, expected, template};
}

// Saved template provenance: reviewed rights and the saved template definitions a generated template snippet names.
function templateContent(saved:RecognitionDocument|null) {
  return saved === null ? null : {rights: saved.template_rights,
    templates: saved.definitions.filter(item => item.kind === 'template').map(({id, region, template, saved: crop}) => ({id, region, template, saved: crop}))};
}

function entryInfo(entry:Entry):CollaborationResourceInfo {
  const info:CollaborationResourceInfo = {path: entry.path, kind: entry.kind, version: entry.version};
  if (entry.capture !== undefined) info.capture = entry.capture;
  if (entry.role !== undefined) info.role = entry.role;
  return info;
}

// The main window's single live draft authority. UI actions and collaboration requests read `current()` and change it
// only through `update`, synchronously; `publish` hands each new session to rendering and is never a second writer.
export class AuthoringController {
  #session:AuthoringSession|null = null;
  readonly #publish:(session:AuthoringSession|null) => void;
  // Versions are unique across controller instances (a reloaded window) and never reused within one.
  readonly #epoch = crypto.randomUUID();
  #counter = 0;
  #serial = 0;
  #lineage:Lineage|null = null;
  // The same package directory keeps one opaque identity across close and reopen in this window; owners and versions
  // do not. Least recently opened directories are forgotten beyond the bound and simply receive a new identity.
  readonly #packages = new Map<string,string>();

  constructor(publish:(session:AuthoringSession|null) => void) {
    this.#publish = publish;
  }

  readonly current = ():AuthoringSession|null => this.#session;

  readonly update = (change:(session:AuthoringSession|null) => AuthoringSession|null):void => {
    const previous = this.#session;
    const next = change(previous);
    if (next === previous) return;
    this.#session = next;
    this.#track(previous, next);
    this.#publish(next);
  };

  // Handles one claimed request synchronously against the current session, so an owner, package or version that
  // changed after the request was sent is refused here rather than at message arrival.
  readonly handle = (value:unknown, editBlock?:CollaborationEditBlock):CollaborationResponse => {
    const request = parseRequest(value);
    if ('code' in request) return this.#failure(request);
    const operation = request.operation;
    if (operation.kind === 'describe') return {owner: this.#lineage?.owner ?? null, ok: true, result: this.describe()};
    if (operation.kind === 'notices') return {owner: this.#lineage?.owner ?? null, ok: true, result: this.#notices(request, operation)};
    const session = this.#session, lineage = this.#lineage;
    if (!session || !lineage) return this.#failure(refusal('no_edit_owner', 'No package is open for editing'));
    const scope = this.checkScope(request.owner, request.package);
    if (scope !== null) return this.#failure(scope);
    const result = operation.kind === 'read' ? this.#read(session, lineage, operation) : this.#edit(session, lineage, operation, editBlock);
    return 'code' in result ? this.#failure(result) : {owner: lineage.owner, ok: true, result};
  };

  // Current metadata for discovery and host-operation coordination; it is usable without an Edit owner.
  readonly describe = ():CollaborationDescription => {
    const session = this.#session, lineage = this.#lineage;
    if (!session || !lineage) {
      return {protocol: COLLABORATION_PROTOCOL, sdk: COLLABORATION_SDK, owner: null, package: null, packageId: null, savedRevision: null,
        selected: null, resources: [], pending: null, conflict: false, refreshRequired: false, limits: LIMITS, cursor: null, validation: null, recognition: null};
    }
    const resources:CollaborationResource[] = draftList(session).map(draft => {
      const entry = lineage.entries.get(key({type: 'file', path: draft.path}))!;
      return {id: entry.id, path: draft.path, kind: draft.kind, version: entry.version, dirty: fileDirty(draft), composing: draft.composing !== null,
        missing: draft.missing};
    });
    const listed = (target:Target, dirty:boolean) => {
      const entry = lineage.entries.get(key(target))!;
      resources.push({id: entry.id, ...entryInfo(entry), dirty, composing: false, missing: false});
    };
    const state = session.recognition;
    let recognition:CollaborationRecognition|null = null;
    if (state) {
      listed({type: 'context'}, recognitionDirty(state));
      for (const {capture_id: capture, document} of captureList(state)) {
        const saved = savedDocument(state, capture);
        listed({type: 'basis', capture}, saved === null || !sameJson(saved.basis, document.basis));
        listed({type: 'template', capture}, false);
        for (const definition of document.definitions) {
          const stored = saved?.definitions.find(item => item.id === definition.id);
          listed({type: 'definition', capture, definition: definition.id}, !stored || !sameJson(definitionContent(stored), definitionContent(definition)));
        }
      }
      const frame = state.view.frame;
      recognition = {capture: state.view.capture_id, frame: frame === null ? null : {width: frame.width, height: frame.height},
        confirmed: geometryConfirmed(state), maxOcrZones: state.view.capabilities.max_ocr_zones};
    }
    const result = session.validation;
    let validation:CollaborationValidation|null = null;
    if (result !== null) {
      listed({type: 'validation'}, false);
      validation = {revision: result.revision, valid: result.valid, current: result.revision === session.revision, diagnostics: result.diagnostics.length,
        excluded: resources.filter(item => item.dirty && (item.role === undefined || item.role === 'context')).map(item => item.id)};
    }
    const selected = session.destination === 'file' && session.selected !== null
      ? lineage.entries.get(key({type: 'file', path: session.selected}))?.id ?? null : null;
    return {protocol: COLLABORATION_PROTOCOL, sdk: COLLABORATION_SDK, owner: lineage.owner, package: lineage.package, packageId: session.packageId,
      savedRevision: session.revision, selected, resources, pending: session.pending?.kind ?? null, conflict: session.conflict,
      refreshRequired: session.refreshRequired, limits: LIMITS, cursor: this.#cursor(lineage, lineage.seq), validation, recognition};
  };

  // Null when the owner and package are exactly the current scope (both null only while no package is open).
  readonly checkScope = (owner:string|null, packageId:string|null):CollaborationError|null => {
    const lineage = this.#lineage;
    if (!this.#session || !lineage) return owner === null && packageId === null ? null : refusal('no_edit_owner', 'No package is open for editing');
    if (owner !== lineage.owner) return refusal('stale_owner', 'The Edit owner changed; describe again');
    if (packageId !== lineage.package) return refusal('stale_package', 'The package changed; describe again');
    return null;
  };

  // The current identity of one resource for a coordinator capturing tickets; never a writable draft.
  readonly resource = (id:string):CollaborationResourceInfo|null => {
    const entry = this.#session ? this.#lineage?.ids.get(id) : undefined;
    return entry?.present ? entryInfo(entry) : null;
  };

  // The current versions of exactly the resources a snippet of `flavor` reads: the capture basis for setup, each
  // definition for grouped OCR, and the definition, basis and saved template provenance for a template. UI checkbox
  // and row selection, frames and trials are not snippet inputs.
  readonly recognitionDependencies = (captureId:string, ids:readonly string[], flavor:string):CollaborationDependency[]|CollaborationError => {
    const session = this.#session, lineage = this.#lineage;
    if (!session || !lineage) return refusal('no_edit_owner', 'No package is open for editing');
    const state = session.recognition;
    if (!state) return refusal('ineligible', 'Recognition is not loaded in the desktop');
    const capture = captureList(state).find(item => item.capture_id === captureId);
    if (!capture) return refusal('unknown_resource', 'The capture is not part of this Edit session');
    const count = flavor === 'game_content' ? ids.length === 0 : flavor === 'ocr_recognize' ? ids.length > 0
      : flavor === 'template_recognize' ? ids.length === 1 : false;
    if (!count || new Set(ids).size !== ids.length) return invalid('The snippet purpose and its distinct definition IDs do not match');
    const current = (target:Target):CollaborationDependency => {
      const entry = lineage.entries.get(key(target))!;
      return {resource: entry.id, version: entry.version};
    };
    const definitions:CollaborationDependency[] = [];
    for (const id of ids) {
      if (!capture.document.definitions.some(item => item.id === id)) return refusal('unknown_resource', 'A definition is not part of the capture');
      definitions.push(current({type: 'definition', capture: captureId, definition: id}));
    }
    const basis = current({type: 'basis', capture: captureId});
    if (flavor === 'game_content') return [basis];
    return flavor === 'ocr_recognize' ? definitions : [...definitions, basis, current({type: 'template', capture: captureId})];
  };

  #failure(error:CollaborationError):CollaborationResponse {
    return {owner: this.#lineage?.owner ?? null, ok: false, error};
  }

  #version():string {
    this.#counter += 1;
    return `${this.#epoch}.${this.#counter}`;
  }

  #cursor(lineage:Lineage, seq:number):string {
    return `${this.#epoch}.${lineage.serial}.${seq}`;
  }

  #packageIdentity(path:string):string {
    const id = this.#packages.get(path) ?? crypto.randomUUID();
    this.#packages.delete(path);
    this.#packages.set(path, id);
    for (const oldest of this.#packages.keys()) {
      if (this.#packages.size <= PACKAGE_IDENTITIES) break;
      this.#packages.delete(oldest);
    }
    return id;
  }

  #entry(lineage:Lineage, target:Target, path:string, kind:string, role?:CollaborationRole, capture?:string):Entry {
    const name = key(target);
    let entry = lineage.entries.get(name);
    if (entry === undefined) {
      entry = {id: crypto.randomUUID(), version: '', target, present: false, signature: null, path, kind, role, capture};
      lineage.entries.set(name, entry);
      lineage.ids.set(entry.id, entry);
    }
    return entry;
  }

  #touch(lineage:Lineage, entry:Entry, notify:boolean) {
    entry.version = this.#version();
    entry.present = true;
    if (notify) this.#notice(lineage, entry);
  }

  #remove(lineage:Lineage, entry:Entry|undefined, notify:boolean) {
    if (!entry?.present) return;
    entry.present = false;
    entry.signature = null;
    if (notify) this.#notice(lineage, entry);
  }

  // Logical resources change version exactly when their content signature differs from the one last issued.
  #settle(lineage:Lineage, entry:Entry, signature:string, notify:boolean) {
    if (entry.present && entry.signature === signature) return;
    entry.signature = signature;
    this.#touch(lineage, entry, notify);
  }

  #notice(lineage:Lineage, entry:Entry) {
    lineage.seq += 1;
    lineage.changes.delete(entry.id);
    lineage.changes.set(entry.id, lineage.seq);
    if (lineage.changes.size <= NOTICE_HISTORY) return;
    const [oldest, seq] = lineage.changes.entries().next().value!;
    lineage.changes.delete(oldest);
    lineage.floor = seq;
  }

  // Every session change passes here, so a resource changed and later restored (ABA) always carries a new version and
  // a notice, whether the person or an agent changed it. Caret moves, selection, UI checkboxes and saves of the same
  // text keep versions. A new lineage issues versions without notices: earlier cursors are invalid anyway.
  #track(previous:AuthoringSession|null, next:AuthoringSession|null) {
    if (next === null) {
      this.#lineage = null;
      return;
    }
    let lineage = this.#lineage;
    let before = previous;
    let notify = true;
    if (lineage === null || before === null || !sameAuthoringRef(before.owner, next.owner)) {
      this.#serial += 1;
      lineage = {serial: this.#serial, owner: next.owner.token, package: this.#packageIdentity(next.packagePath), packagePath: next.packagePath,
        entries: new Map(), ids: new Map(), seq: 0, floor: 0, changes: new Map(), recognition: null, validation: null, validationPath: ''};
      this.#lineage = lineage;
      before = null;
      notify = false;
    } else if (lineage.packagePath !== next.packagePath) {
      lineage.package = this.#packageIdentity(next.packagePath);
      lineage.packagePath = next.packagePath;
    }
    for (const [path, draft] of next.drafts) {
      const entry = this.#entry(lineage, {type: 'file', path}, path, draft.kind);
      const old = before?.drafts.get(path);
      if (entry.present && old === draft) continue;
      if (!entry.present || old === undefined || old.text !== draft.text || old.kind !== draft.kind || old.missing !== draft.missing || draft.text === null
        || old.typed.length !== draft.typed.length || old.typed.some((item, index) => item.path !== draft.typed[index].path || item.text !== draft.typed[index].text)) {
        entry.kind = draft.kind;
        this.#touch(lineage, entry, notify);
      }
    }
    if (before !== null) {
      for (const path of before.drafts.keys()) if (!next.drafts.has(path)) this.#remove(lineage, lineage.entries.get(key({type: 'file', path})), notify);
    }
    this.#trackRecognition(lineage, next.recognition, notify);
    if (next.validation !== lineage.validation || next.packagePath !== lineage.validationPath) {
      lineage.validation = next.validation;
      lineage.validationPath = next.packagePath;
      const entry = this.#entry(lineage, {type: 'validation'}, 'validation', 'validation');
      if (next.validation === null) this.#remove(lineage, entry, notify);
      else this.#touch(lineage, entry, notify);
    }
  }

  // Recognition versions follow content: the basis value, each definition's source inputs, saved template provenance,
  // and for the savable aggregate every capture's draft document, the active capture and its pending crops. Frames,
  // confirmation, trials, checked rows, selection, display and the saved package revision are not part of any version.
  #trackRecognition(lineage:Lineage, state:RecognitionState|null, notify:boolean) {
    const tracked = lineage.recognition;
    if (state === tracked) return;
    lineage.recognition = state;
    if (state === null) {
      for (const entry of lineage.entries.values()) if (entry.target.type !== 'file' && entry.target.type !== 'validation') this.#remove(lineage, entry, notify);
      return;
    }
    if (tracked !== null && tracked.view === state.view && tracked.document === state.document && tracked.cropIds === state.cropIds) return;
    const live = new Set<Entry>();
    const captures:Json[] = [];
    for (const {capture_id: capture, document} of captureList(state)) {
      const prefix = `recognition/${capture}`;
      const basis = this.#entry(lineage, {type: 'basis', capture}, `${prefix}/basis`, 'recognition_basis', 'basis', capture);
      this.#settle(lineage, basis, JSON.stringify(document.basis), notify);
      const template = this.#entry(lineage, {type: 'template', capture}, `${prefix}/template`, 'recognition_template', 'template', capture);
      this.#settle(lineage, template, JSON.stringify(templateContent(savedDocument(state, capture))), notify);
      live.add(basis).add(template);
      const definitions = document.definitions.map(definition => {
        const entry = this.#entry(lineage, {type: 'definition', capture, definition: definition.id}, `${prefix}/definitions/${definition.id}`,
          'recognition_definition', 'definition', capture);
        this.#settle(lineage, entry, JSON.stringify(definitionContent(definition)), notify);
        live.add(entry);
        return [entry.version, JSON.stringify(definition.saved)];
      });
      captures.push([capture, basis.version, JSON.stringify(document.template_rights), definitions]);
    }
    const context = this.#entry(lineage, {type: 'context'}, 'recognition', 'recognition', 'context');
    context.capture = state.view.capture_id ?? undefined;
    this.#settle(lineage, context, JSON.stringify([state.view.capture_id, state.cropIds, captures]), notify);
    live.add(context);
    for (const entry of lineage.entries.values()) {
      if (entry.target.type !== 'file' && entry.target.type !== 'validation' && !live.has(entry)) this.#remove(lineage, entry, notify);
    }
  }

  // The current content of a present resource; null when it no longer exists in the session.
  #content(session:AuthoringSession, lineage:Lineage, entry:Entry):Content|null {
    const target = entry.target;
    if (target.type === 'file') {
      const draft = session.drafts.get(target.path);
      if (!draft) return null;
      return draft.kind === 'source' || draft.typed.length === 0 ? {path: draft.path, text: draft.text} : {path: draft.path, text: draft.text, typed: draft.typed};
    }
    if (target.type === 'validation') {
      const validation = session.validation;
      if (validation === null) return null;
      // Diagnostics name package-relative locations only; the package directory never leaves the desktop.
      const diagnostics = validation.diagnostics.map(fault => {
        const location = diagnosticLocation(fault);
        const context = record(fault.context);
        const code = typeof context?.code === 'string' || typeof context?.code === 'number' ? context.code : null;
        return {category: fault.category, message: session.packagePath ? fault.message.split(session.packagePath).join('<package>') : fault.message,
          path: location && !location.path.startsWith('/') && !location.path.startsWith('~') ? location.path : null,
          line: location?.line ?? null, column: location?.column ?? null, code};
      });
      return {path: entry.path, text: JSON.stringify({revision: validation.revision, valid: validation.valid, diagnostics}, null, 2)};
    }
    const state = session.recognition;
    if (!state) return null;
    const captures = captureList(state);
    if (target.type === 'context') {
      const id = (resource:Target) => lineage.entries.get(key(resource))!.id;
      return {path: entry.path, text: JSON.stringify({activeCapture: state.view.capture_id, crops: state.cropIds,
        captures: captures.map(({capture_id: capture, document}) => ({capture, active: capture === state.view.capture_id, basis: document.basis,
          basisResource: id({type: 'basis', capture}), templateResource: id({type: 'template', capture}), rights: document.template_rights,
          definitions: document.definitions.map(definition => ({resource: id({type: 'definition', capture, definition: definition.id}),
            ...definitionContent(definition), saved: definition.saved, edges: mapRegion(definition.region, document.basis),
            searchEdges: definition.template === null ? null : mapRegion(definition.template.search_region, document.basis)}))}))}, null, 2)};
    }
    const capture = captures.find(item => item.capture_id === target.capture);
    if (!capture) return null;
    if (target.type === 'basis') return {path: entry.path, text: JSON.stringify({capture: target.capture, basis: capture.document.basis}, null, 2)};
    if (target.type === 'template') {
      return {path: entry.path, text: JSON.stringify({capture: target.capture, saved: templateContent(savedDocument(state, target.capture))}, null, 2)};
    }
    const definition = capture.document.definitions.find(item => item.id === target.definition);
    return definition ? {path: entry.path, text: JSON.stringify({capture: target.capture, ...definitionContent(definition)}, null, 2)} : null;
  }

  // Pages never split a non-BMP character, so every page and their concatenation are the exact current text.
  #read(session:AuthoringSession, lineage:Lineage, operation:Extract<Operation, {kind:'read'}>):CollaborationPage|CollaborationError {
    const entry = lineage.ids.get(operation.resource);
    const content = entry?.present ? this.#content(session, lineage, entry) : null;
    if (!entry || !content) return refusal('unknown_resource', 'The resource is not part of this Edit session', operation.resource);
    if (content.text === null) return refusal('unsupported_resource', 'Binary resources have no text', entry.id);
    if (operation.version !== null && operation.version !== entry.version) {
      return refusal('stale_version', 'The resource changed since that version; read it again', entry.id);
    }
    const text = content.text;
    const {offset} = operation;
    if (offset > text.length || splits(text, offset)) return refusal('invalid_range', 'The offset is not a character boundary of the text', entry.id);
    let end = Math.min(text.length, offset + operation.limit);
    if (splits(text, end)) end -= 1;
    if (end === offset && offset < text.length) return refusal('invalid_range', 'The limit cannot hold the character at this offset', entry.id);
    const page:CollaborationPage = {resource: entry.id, path: content.path, version: entry.version, text: text.slice(offset, end), offset,
      nextOffset: end < text.length ? end : null, totalUnits: text.length, savedRevision: session.revision};
    if (content.typed !== undefined) page.typed = content.typed;
    return page;
  }

  // Changes after `cursor`, coalesced to each resource's current state in change order. Notices never adopt an owner:
  // a request for another owner or package gets a gap without a cursor and must describe again.
  #notices(request:Request, operation:Extract<Operation, {kind:'notices'}>):CollaborationNotices {
    const session = this.#session, lineage = this.#lineage;
    if (!session || !lineage || this.checkScope(request.owner, request.package) !== null) {
      return {cursor: null, gap: 'owner', changes: [], more: false, savedRevision: null};
    }
    const head = this.#cursor(lineage, lineage.seq);
    if (operation.cursor === null) return {cursor: head, gap: 'start', changes: [], more: false, savedRevision: session.revision};
    const [epoch, serial, sequence, ...rest] = operation.cursor.split('.');
    const position = epoch === this.#epoch && serial === String(lineage.serial) && rest.length === 0 && /^\d{1,15}$/.test(sequence ?? '')
      ? Number(sequence) : -1;
    if (position < lineage.floor || position > lineage.seq) return {cursor: head, gap: 'expired', changes: [], more: false, savedRevision: session.revision};
    const changes:CollaborationNotice[] = [];
    let last = position;
    for (const [id, seq] of lineage.changes) {
      if (seq <= position) continue;
      if (changes.length === operation.limit) {
        return {cursor: this.#cursor(lineage, last), gap: null, changes, more: true, savedRevision: session.revision};
      }
      const entry = lineage.ids.get(id)!;
      changes.push({resource: entry.id, ...entryInfo(entry), version: entry.present ? entry.version : null});
      last = seq;
    }
    return {cursor: head, gap: null, changes, more: false, savedRevision: session.revision};
  }

  // Why one target cannot take its edit, or null.
  #admits(session:AuthoringSession, entry:Entry, edit:TargetEdit):CollaborationError|null {
    const target = entry.target;
    const draft = target.type === 'file' ? session.drafts.get(target.path) : undefined;
    if ('fields' in edit) {
      const domain = FIELD_DOMAINS[target.type === 'file' ? draft!.kind : target.type];
      if (domain === undefined) return refusal('unsupported_resource', 'This resource accepts no field edits', entry.id);
      if (edit.fields.some(field => fieldDomain(field) !== domain)) return refusal('unsupported_field', `Only ${domain} fields apply to this resource`, entry.id);
    } else if (draft === undefined || draft.kind !== 'source' || draft.text === null) {
      return refusal('unsupported_resource', 'Only script sources accept text edits', entry.id);
    }
    if (draft !== undefined) {
      if (draft.missing) return refusal('missing_resource', 'The file is no longer declared by the package', entry.id);
      if (draft.composing !== null) return refusal('composing', 'Text input is being composed in this file; try again after it ends', entry.id);
      return null;
    }
    // Recognition edits change the active capture's loaded draft only; another capture is never selected implicitly.
    const state = session.recognition;
    const active = state?.view.capture_id ?? null;
    if (!state || state.document === null || active === null || ('capture' in target && target.capture !== active)) {
      return refusal('ineligible', "Only the active capture's loaded recognition draft is editable; select it in the desktop", entry.id);
    }
    return null;
  }

  // Validates every target, dependency, field, range, IME state and byte budget before changing anything; then applies
  // the whole batch in one synchronous transition: one separate undo step per changed file and the existing bounded
  // recognition Undo and freshness rules for recognition fields.
  #edit(session:AuthoringSession, lineage:Lineage, operation:Extract<Operation, {kind:'edit'}>,
    editBlock:CollaborationEditBlock|undefined):CollaborationEditResult|CollaborationError {
    // Catalog publication, Refresh, Exit and Duplicate replace or rebase drafts; Save keeps later edits dirty instead.
    const pending = session.pending?.kind;
    if (pending === 'catalog' || pending === 'refresh' || pending === 'exit' || pending === 'duplicate') {
      return refusal('ineligible', `The Edit session is busy (${pending}); describe again after it settles`);
    }
    const recognition = operation.edits.some(edit => {
      const type = lineage.ids.get(edit.resource)?.target.type;
      return type !== undefined && type !== 'file' && type !== 'validation';
    });
    const blocked = editBlock?.(session, recognition) ?? null;
    if (blocked !== null) return refusal('ineligible', blocked);
    const targets:Entry[] = [];
    for (const edit of operation.edits) {
      const entry = lineage.ids.get(edit.resource);
      // Presence is tracked with every session change, so a present entry names an existing draft or recognition part.
      if (!entry?.present) return refusal('unknown_resource', 'The resource is not part of this Edit session', edit.resource);
      if (edit.version !== entry.version) return refusal('stale_version', 'The resource changed since that version; read it again', entry.id);
      const refused = this.#admits(session, entry, edit);
      if (refused !== null) return refused;
      targets.push(entry);
    }
    for (const dependency of operation.dependencies) {
      const entry = lineage.ids.get(dependency.resource);
      if (!entry?.present) return refusal('unknown_resource', 'The dependency is not part of this Edit session', dependency.resource);
      if (dependency.version !== entry.version) return refusal('stale_version', 'A dependency changed since that version; read it again', entry.id);
    }
    let candidate = session;
    const changed:string[] = [];
    const created:string[] = [];
    for (const [index, edit] of operation.edits.entries()) {
      const entry = targets[index];
      const target = entry.target;
      if (target.type !== 'file') {
        const state = candidate.recognition!;
        const role:RecognitionTarget = target.type === 'definition' ? {role: 'definition', id: target.definition}
          : target.type === 'basis' ? {role: 'basis'} : {role: 'context'};
        const patched = patchRecognition(state, role, 'fields' in edit ? edit.fields : []);
        if ('code' in patched) return refusal(patched.code, patched.message, entry.id);
        if (patched.state !== state) candidate = {...candidate, recognition: patched.state};
        created.push(...patched.created);
        continue;
      }
      const draft = candidate.drafts.get(target.path)!;
      const original = draft.text!;
      let text:string;
      let range = draft.range;
      if ('fields' in edit) {
        // Presets follow the current options schema draft, including an earlier target of this batch.
        const schema = draftList(candidate).find(item => item.kind === 'schema' && !item.missing)?.text ?? null;
        const patched = patchMetadata(draft.kind as 'manifest'|'schema'|'profile', original, draft.base, edit.fields, {packageId: candidate.packageId, schema});
        if (typeof patched !== 'string') return refusal(patched.code, patched.message, entry.id);
        text = patched;
      } else {
        let splices:Splice[];
        if ('text' in edit) {
          text = edit.text;
          splices = [sourceDifference(original, text)];
        } else {
          const applied = spliceRanges(original, edit.ranges);
          if (typeof applied === 'string') return refusal('invalid_range', applied, entry.id);
          ({text, splices} = applied);
        }
        range = {start: mapPosition(draft.range.start, splices), end: mapPosition(draft.range.end, splices)};
      }
      if (text === original) continue;
      if (utf8Bytes(text) > AUTHORING_SOURCE_BYTES) {
        return refusal('source_too_large', `The file would exceed ${AUTHORING_SOURCE_BYTES} UTF-8 bytes`, entry.id);
      }
      candidate = applyExternalEdits(candidate, [{path: target.path, text, range}]);
      changed.push(target.path);
    }
    if (changed.length > 0) {
      const first = candidate.drafts.get(changed[0])!;
      if (utf8Bytes(first.text!) + otherNonImageBytes(candidate, first.path) > AUTHORING_NON_IMAGE_BYTES) {
        return refusal('package_too_large', `The package's non-image files would exceed ${AUTHORING_NON_IMAGE_BYTES} UTF-8 bytes`);
      }
    }
    // Nothing yielded since `session` was read, so this commits exactly the validated transition.
    if (candidate !== session) this.update(current => current === session ? candidate : current);
    const active = candidate.recognition?.view.capture_id ?? '';
    return {resources: targets.map(entry => ({resource: entry.id, version: entry.present ? entry.version : null})),
      created: created.map(id => {
        const entry = lineage.entries.get(key({type: 'definition', capture: active, definition: id}))!;
        return {resource: entry.id, path: entry.path, version: entry.version};
      }), savedRevision: session.revision};
  }
}

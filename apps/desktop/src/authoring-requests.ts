import {SDK_SEARCH_LIMITS} from '../../../tools/runtime-comparison/compiler/sdk.mjs';
import type {SdkDetailResult, SdkIdentity, SdkSearchResult} from '../../../tools/runtime-comparison/compiler/sdk.mjs';
import type {CollaborationDependency, CollaborationError} from './authoring-controller.ts';
import type {SaveTarget} from './authoring-publication.ts';
import type {CatalogEdit, Fault} from './types.ts';
import type {GeometryBasis, SnippetKind, SnippetVerification} from './recognition.ts';

export type Resolution = 'save'|'discard'|'cancel';
export type HostOperation =
  | {kind:'save'; target:SaveTarget}
  | {kind:'save_all'; targets?:SaveTarget[]}
  | {kind:'catalog_add'; revision:string; edit:Extract<CatalogEdit,{kind:'add'}>}
  | {kind:'catalog_rename'; revision:string; edit:Extract<CatalogEdit,{kind:'rename'}>}
  | {kind:'catalog_remove'; revision:string; edit:Extract<CatalogEdit,{kind:'remove'}>}
  | {kind:'refresh'}
  | {kind:'validate'; revision:string}
  | {kind:'cancel'}
  | {kind:'create'; workspace:string; packageId:string; resolution?:Resolution}
  | {kind:'open'; workspace:string; path:string; resolution?:Resolution}
  | {kind:'duplicate'; packageId:string; resolution?:Resolution}
  | {kind:'exit'; resolution?:Resolution}
  | {kind:'sdk_search'; query:string; cursor?:string; limit?:number}
  | {kind:'sdk_detail'; name:string}
  | {kind:'snippet'; capture:string; mode:SnippetKind; ids:string[]};

export type HostFailure = Pick<CollaborationError, 'code'|'message'|'outcome'>;
export interface PublicationResult {
  complete:boolean; committed:{path:string; revision:string}[]; remaining:SaveTarget[];
  savedRevision:string|null; refreshRequired:boolean; failure:HostFailure|null;
}
export interface LifecycleResult {
  complete:boolean; connection:{owner:string|null; package:string|null}; previousReleased:boolean;
  publication:PublicationResult|null; failure:HostFailure|null;
}
export interface SnippetResult {
  source:string; sdk:SdkIdentity;
  receipt:{owner:string; package:string; capture:string; ids:string[]; basis:GeometryBasis; revision:string;
    dependencies:CollaborationDependency[]; verification:SnippetVerification};
}
export type HostResult = SdkSearchResult|SdkDetailResult|PublicationResult|LifecycleResult|SnippetResult
  | {complete:boolean; savedRevision:string|null; refreshRequired:boolean}
  | {complete:true; revision:string; valid:boolean; current:boolean; excluded:string[];
      diagnostics:{count:number; resource:string|null; version:string|null}}
  | {requested:boolean; settled:false};

// Host faults can contain local filesystem/configuration paths. Keep the category, not their private detail.
export function hostFailure(error:Fault|null, message:string, outcome?:'unknown'):HostFailure {
  const category = error && /^[A-Za-z][A-Za-z0-9_]{0,50}$/.test(error.category)
    ? error.category.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase() : 'operation_failed';
  return {code:`host_${category}`, message, ...(outcome === undefined ? {} : {outcome})};
}

const HOST_OPERATIONS:Record<string,true> = {save:true, save_all:true, catalog_add:true, catalog_rename:true,
  catalog_remove:true, refresh:true, validate:true, cancel:true, create:true, open:true, duplicate:true, exit:true,
  sdk_search:true, sdk_detail:true, snippet:true};
const IDENTITY = /^[!-~]{1,256}$/;
const RESOLUTIONS:Record<string,true> = {save:true, discard:true, cancel:true};
const SNIPPETS:Record<string,true> = {game_content:true, ocr_recognize:true, template_recognize:true};
export const SAVE_TARGETS = 256;

export function requestError(code:string, message:string, resource?:string):CollaborationError {
  return {code, message, outcome:'not_applied', ...(resource === undefined ? {} : {resource})};
}

function record(value:unknown):value is Record<string,unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function identity(value:unknown):value is string {
  return typeof value === 'string' && IDENTITY.test(value);
}
function text(value:unknown, limit:number, empty = false):value is string {
  return typeof value === 'string' && (empty || value.length > 0) && value.length <= limit && !value.includes('\u0000');
}
function only(value:Record<string,unknown>, keys:readonly string[]):boolean {
  return Object.keys(value).every(key => keys.includes(key));
}
function target(value:unknown):value is SaveTarget {
  return record(value) && only(value, ['resource','version']) && identity(value.resource) && identity(value.version);
}

// Null delegates to the synchronous draft controller; all host operations have a closed shape.
export function parseHostOperation(value:unknown):HostOperation|CollaborationError|null {
  if (!record(value) || typeof value.kind !== 'string' || !Object.hasOwn(HOST_OPERATIONS, value.kind)) return null;
  const invalid = () => requestError('invalid_request', 'The authoring operation arguments are invalid');
  const {kind} = value;
  if (kind === 'refresh' || kind === 'cancel') return only(value, ['kind']) ? {kind} : invalid();
  if (kind === 'save') {
    if (!only(value, ['kind','resource','version']) || !identity(value.resource) || !identity(value.version)) return invalid();
    return {kind, target:{resource:value.resource, version:value.version}};
  }
  if (kind === 'save_all') {
    if (!only(value, ['kind','resources'])) return invalid();
    if (value.resources === undefined) return {kind};
    if (!Array.isArray(value.resources) || value.resources.length < 1 || value.resources.length > SAVE_TARGETS
      || !value.resources.every(target) || new Set(value.resources.map(item => item.resource)).size !== value.resources.length) return invalid();
    return {kind, targets:value.resources};
  }
  if (kind === 'validate') return only(value, ['kind','revision']) && identity(value.revision) ? {kind, revision:value.revision} : invalid();
  if (kind === 'sdk_detail') return only(value, ['kind','name']) && text(value.name, 128) ? {kind, name:value.name} : invalid();
  if (kind === 'sdk_search') {
    if (!only(value, ['kind','query','cursor','limit']) || !text(value.query, SDK_SEARCH_LIMITS.queryUnits, true)
      || (value.cursor !== undefined && !text(value.cursor, 4096))
      || (value.limit !== undefined && (typeof value.limit !== 'number' || !Number.isInteger(value.limit) || value.limit < 1 || value.limit > SDK_SEARCH_LIMITS.maxLimit))) return invalid();
    return {kind, query:value.query, ...(value.cursor === undefined ? {} : {cursor:value.cursor as string}),
      ...(value.limit === undefined ? {} : {limit:value.limit as number})};
  }
  if (kind === 'snippet') {
    if (!only(value, ['kind','capture','mode','ids']) || !text(value.capture, 256) || typeof value.mode !== 'string' || !Object.hasOwn(SNIPPETS, value.mode)
      || !Array.isArray(value.ids) || value.ids.length > 256 || !value.ids.every(id => text(id, 256)) || new Set(value.ids).size !== value.ids.length) return invalid();
    return {kind, capture:value.capture, mode:value.mode as SnippetKind, ids:value.ids};
  }
  if (kind === 'catalog_add') {
    if (!only(value, ['kind','revision','path','fileKind','text','id','module','format','width','height']) || !identity(value.revision)
      || !text(value.path, 4096) || typeof value.fileKind !== 'string' || !['source','profile','asset','source_map'].includes(value.fileKind)
      || typeof value.text !== 'string' || value.text.length > 1_048_576) return invalid();
    for (const key of ['id','module','format']) if (value[key] !== undefined && !text(value[key], 4096)) return invalid();
    for (const key of ['width','height']) if (value[key] !== undefined && (typeof value[key] !== 'number' || !Number.isInteger(value[key]) || value[key] < 0 || value[key] > 4_294_967_295)) return invalid();
    const edit:CatalogEdit = {kind:'add', path:value.path, file_kind:value.fileKind as 'source'|'profile'|'asset'|'source_map', text:value.text};
    if (typeof value.id === 'string') edit.id = value.id;
    if (typeof value.module === 'string') edit.module = value.module;
    if (typeof value.format === 'string') edit.format = value.format;
    if (typeof value.width === 'number') edit.width = value.width;
    if (typeof value.height === 'number') edit.height = value.height;
    return {kind, revision:value.revision, edit};
  }
  if (kind === 'catalog_rename' || kind === 'catalog_remove') {
    if (!only(value, kind === 'catalog_rename' ? ['kind','revision','path','destination'] : ['kind','revision','path'])
      || !identity(value.revision) || !text(value.path, 4096) || (kind === 'catalog_rename' && !text(value.destination, 4096))) return invalid();
    return kind === 'catalog_rename'
      ? {kind, revision:value.revision, edit:{kind:'rename', path:value.path, destination:value.destination as string}}
      : {kind, revision:value.revision, edit:{kind:'remove', path:value.path}};
  }
  if (value.resolution !== undefined && (typeof value.resolution !== 'string' || !Object.hasOwn(RESOLUTIONS, value.resolution))) return invalid();
  const resolution = value.resolution as Resolution|undefined;
  if (kind === 'exit') return only(value, ['kind','resolution']) ? {kind, resolution} : invalid();
  if (kind === 'duplicate') return only(value, ['kind','packageId','resolution']) && text(value.packageId, 256) ? {kind, packageId:value.packageId, resolution} : invalid();
  if (kind === 'create') return only(value, ['kind','workspace','packageId','resolution']) && identity(value.workspace) && text(value.packageId, 256)
    ? {kind, workspace:value.workspace, packageId:value.packageId, resolution} : invalid();
  if (kind === 'open') return only(value, ['kind','workspace','path','resolution']) && identity(value.workspace) && text(value.path, 4096)
    ? {kind, workspace:value.workspace, path:value.path, resolution} : invalid();
  return invalid();
}

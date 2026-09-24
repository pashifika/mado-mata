import {LocalFault, messages} from './i18n.ts';
import type {Locale, Message} from './i18n.ts';
import {defaultDraft, readDraft} from './state.ts';
import type {ControllerView, Fault, Json, LegacyImport, LogEntry, OcrEnvironment, PackageInfo, Profile, Selection, WorkspaceRef, WorkspaceView} from './types.ts';

export const WORKSPACE_LIMIT = 8;
export const SAVED_LIMIT = 64;
export const CLOSED_LIMIT = 16;
export const INTERNAL_NAME_LIMIT = 64;
export const DISPLAY_NAME_LIMIT = 80;
// The host caps a retained check descriptor; refuse longer paths inline instead of after a round trip.
export const DESCRIPTOR_LIMIT = 4096;
// The host's category for a saved custom-archive reference; every other source fault is an unavailable directory.
export const UNSUPPORTED_SOURCE = 'UnsupportedPackageSource';
const BUSY_PHASES: Record<string, true> = {preparing: true, running: true, stopping: true};
const INTERNAL_NAME = /^[A-Za-z_-]{1,64}$/;
const CONTROL = /\p{Cc}/u;

// The three lifecycle phases during which the runner is reserved by one operation.
export function busy(state:string):boolean {
  return BUSY_PHASES[state] === true;
}

export type Page = 'run' | 'logs';

// Package-bound session state. Present only after real inspection in this session or host revalidation at bootstrap.
export interface Bound {
  packagePath:string; package:PackageInfo;
  // Last readable catalog of this Tab/package store and its current listing fault.
  profiles:Profile[]; profilesError:Fault|null;
  selectedId:string|null; name:string; preset:string; draft:Record<string,Json>;
  // Local edit counter: a validation or save that raced a later edit must not overwrite it.
  draftRevision:number; validation:Record<string,Json>|null;
  lane:string; scenario:string; descriptorPath:string; disclosedRun:string|null;
  // True once the operator changed profile/draft state; a pristine default draft closes without confirmation.
  touched:boolean;
}

// Session-local UI state for one open named Tab. The host persists names and package references; nothing here is.
export interface Workspace {
  id:string; revision:number; internalName:string; displayName:string;
  bound:Bound|null;
  // Host-reported reason the saved source could not be inspected. Null together with `bound` null means no saved package.
  sourceError:Fault|null;
  page:Page; error:Fault|null; notice:Message|null; logFilter:LogFilter;
  // Label of the in-flight state-changing command owned by this workspace, if any.
  busy:Message|null;
  // Inspect form draft for this Tab; a path is only a request, never a binding.
  inspectPath:string;
  legacyImport:LegacyImport|null;
}

export type BoundWorkspace = Workspace & {bound:Bound};

// Retained after close so diagnostics stay attributable without reopening the Tab.
export interface ClosedWorkspace {id:string; revision:number; label:string; result:ControllerView|null}

export interface Origin {id:string; revision:number; draftRevision?:number}

export type ProfileCatalog = Pick<Selection,'profiles'|'profiles_error'>;
export interface WorkspaceCommand {
  update:(workspace:Workspace) => Workspace;
  catalog?:ProfileCatalog;
}

export function isBound(workspace:Workspace):workspace is BoundWorkspace {
  return workspace.bound !== null;
}

export type NameError = 'internalEmpty' | 'internalLong' | 'internalChars' | 'displayBlank' | 'displayLong' | 'displayControl';

// Mirrors the host rule `^[A-Za-z_-]{1,64}$`: no digits, no trimming, no automatic suffix.
export function internalNameError(value:string):NameError|null {
  if (value === '') return 'internalEmpty';
  if (value.length > INTERNAL_NAME_LIMIT) return 'internalLong';
  return INTERNAL_NAME.test(value) ? null : 'internalChars';
}

// Display names count Unicode scalar values, not UTF-16 units, and are stored exactly as entered.
export function displayNameError(value:string):NameError|null {
  if (value.trim() === '') return 'displayBlank';
  if (Array.from(value).length > DISPLAY_NAME_LIMIT) return 'displayLong';
  return CONTROL.test(value) ? 'displayControl' : null;
}

function fromSelection(selection:Selection, previous?:Bound):Bound {
  return {
    packagePath: selection.package_path, package: selection.package,
    profiles: selection.profiles, profilesError: selection.profiles_error,
    selectedId: null, name: '', preset: '', draft: defaultDraft(selection.package.schema),
    draftRevision: (previous?.draftRevision ?? 0) + 1, validation: null,
    lane: previous?.lane ?? 'controlled', scenario: previous?.scenario ?? 'workflow', descriptorPath: previous?.descriptorPath ?? '',
    disclosedRun: null, touched: false,
  };
}

// A host view becomes a session: unbound, unusable saved source, or bound to real inventory. No draft is restored.
export function workspaceFromView(view:WorkspaceView, notice:Message|null = null):Workspace {
  return {
    id: view.workspace_id, revision: view.revision, internalName: view.internal_name, displayName: view.display_name,
    bound: view.selection ? fromSelection(view.selection) : null, sourceError: view.source_error,
    page: 'run', error: null, notice, logFilter: {text: '', level: ''}, busy: null,
    inspectPath: view.selection?.package_path ?? '', legacyImport: null,
  };
}

// Binding publishes the host's new revision; execution configuration and navigation survive, package-bound draft
// state does not. A previous source error is resolved by the successful inspection.
export function bindSelection(workspace:Workspace, selection:Selection, notice:Message|null = null):Workspace {
  return {
    ...workspace, revision: selection.revision, bound: fromSelection(selection, workspace.bound ?? undefined), sourceError: null,
    error: null, notice, inspectPath: selection.package_path, legacyImport: null,
  };
}

// The host lists the whole store or nothing: one malformed, oversized, or unsupported file fails the entire listing,
// which then arrives as `[]` plus a fault. That says nothing about the profiles themselves, so the last known
// catalog, selection, and draft stay until a later read succeeds. Only `ProfileRejected` is a real, partial catalog:
// the compatible profiles are listed and the rejected ones are named in the fault.
function catalogKnown(error:Fault|null):boolean {
  return error === null || error.category === 'ProfileRejected';
}

// Takes a fresh catalog without touching draft values. A selected profile that changed on disk leaves the local
// values as an explained draft, so Start never carries an identity the store no longer matches; the profile name
// follows a rename only while it was not edited locally, so a stale name cannot undo the rename on the next save.
function reconcileProfiles(bound:Bound, profiles:Profile[]):Bound {
  const known = bound.profiles;
  if (known === profiles || (known.length === profiles.length && JSON.stringify(known) === JSON.stringify(profiles))) return bound;
  const previous = known.find(item => item.id === bound.selectedId);
  if (!previous) return {...bound, profiles};
  const current = profiles.find(item => item.id === previous.id);
  if (!current) {
    return {...bound, profiles, selectedId: null, touched: true};
  }
  const renamed = previous.name !== current.name;
  return {...bound, profiles, name: renamed && bound.name === previous.name ? current.name : bound.name};
}

function catalogNotice(before:Bound, after:Bound):Message|null {
  const previous = before.profiles.find(item => item.id === before.selectedId);
  if (!previous || before.profiles === after.profiles) return null;
  const current = after.profiles.find(item => item.id === previous.id);
  if (!current) return {key: 'deletedElsewhere', args: [previous.name]};
  if (JSON.stringify(previous.values) !== JSON.stringify(current.values)) return {key: 'updatedElsewhere', args: [current.name]};
  if (previous.name !== current.name) return {key: 'renamedElsewhere', args: [current.name]};
  return null;
}

// A catalog belongs to exactly one Tab/package store. Other Tabs referencing the same package keep their own files
// and are never touched; a failed read keeps the last catalog and reports only the fault.
export function applyCatalog(workspace:Workspace, catalog:ProfileCatalog):Workspace {
  const bound = workspace.bound;
  if (!bound) return workspace;
  if (!catalogKnown(catalog.profiles_error)) {
    return bound.profilesError === catalog.profiles_error ? workspace : {...workspace, bound: {...bound, profilesError: catalog.profiles_error}};
  }
  const reconciled = reconcileProfiles(bound, catalog.profiles);
  const next = reconciled.profilesError === catalog.profiles_error ? reconciled : {...reconciled, profilesError: catalog.profiles_error};
  if (next === bound) return workspace;
  const notice = catalogNotice(bound, next);
  return {...workspace, bound: next, notice: notice ?? workspace.notice};
}

export function closeWorkspace(list:Workspace[], id:string):Workspace[] {
  return list.filter(item => item.id !== id);
}

export function retainClosed(closed:ClosedWorkspace[], entry:ClosedWorkspace):ClosedWorkspace[] {
  return [...closed.filter(item => item.id !== entry.id), entry].slice(-CLOSED_LIMIT);
}

// A completion is applied only to the workspace/revision that started it; otherwise it is obsolete.
export function applyIfCurrent(list:Workspace[], origin:Origin, update:(workspace:Workspace) => Workspace):Workspace[] {
  const index = list.findIndex(item => item.id === origin.id);
  if (index < 0) return list;
  const current = list[index];
  if (current.revision !== origin.revision) return list;
  if (origin.draftRevision !== undefined && origin.draftRevision !== current.bound?.draftRevision) return list;
  const next = [...list];
  next[index] = update(current);
  return next;
}

// Validation and failed commands carry no fresh listing and cannot republish a stale cache.
export function applyCommand(list:Workspace[], origin:Origin, command:WorkspaceCommand):Workspace[] {
  const next = applyIfCurrent(list, origin, command.update);
  return next === list || !command.catalog ? next : updateWorkspace(next, origin.id, item => applyCatalog(item, command.catalog!));
}

export function updateWorkspace(list:Workspace[], id:string, update:(workspace:Workspace) => Workspace):Workspace[] {
  const index = list.findIndex(item => item.id === id);
  if (index < 0) return list;
  const next = [...list];
  next[index] = update(list[index]);
  return next;
}

// Package-bound edits apply only while the Tab is bound; an unbound Tab has no draft to edit.
export function updateBound(workspace:Workspace, update:(bound:Bound) => Bound):Workspace {
  return workspace.bound ? {...workspace, bound: update(workspace.bound)} : workspace;
}

// Facts derived from a bound draft for the Run page and for command admission.
export interface Derived {
  parsed:{values:Record<string,Json>; errors:Record<string,string>};
  numericErrors:boolean; valuesDirty:boolean; dirty:boolean; bound:boolean;
  selectedProfile:Profile|undefined; startBlock:string|null; descriptorError:string|null;
}

const encoder = new TextEncoder();

export function deriveBound(bound:Bound, savedEnvironment:OcrEnvironment|null, locale:Locale = 'en'):Derived {
  const t = messages[locale].app;
  const parsed = readDraft(bound.package.schema, bound.draft, locale);
  const numericErrors = Object.keys(parsed.errors).length > 0;
  const selectedProfile = bound.profiles.find(profile => profile.id === bound.selectedId);
  const valuesDirty = !selectedProfile || numericErrors || JSON.stringify(parsed.values) !== JSON.stringify(selectedProfile.values);
  const dirty = valuesDirty || bound.name !== selectedProfile?.name;
  const profileBound = !selectedProfile || (selectedProfile.package_id === bound.package.package_id && selectedProfile.schema_identity === bound.package.schema_identity);
  const descriptor = bound.descriptorPath.trim();
  const startBlock = bound.lane === 'replay' && !descriptor ? t.replayDescriptor
    : bound.lane === 'replay' && !savedEnvironment ? t.replayEnvironment : null;
  const descriptorError = encoder.encode(descriptor).length > DESCRIPTOR_LIMIT ? t.descriptorLimit(DESCRIPTOR_LIMIT) : null;
  return {parsed, numericErrors, valuesDirty, dirty, bound: profileBound, selectedProfile, startBlock, descriptorError};
}

// Package commands carry values only from a real inspected selection. A genuine named Tab without one is refused
// here before any host call, exactly as the host refuses it; nothing is synthesized from names or saved references.
export function commandValues(workspace:Workspace, facts:Derived|undefined):Record<string,Json> {
  if (!workspace.bound || !facts) throw new LocalFault({key: 'unboundWorkspace'});
  if (!facts.bound) throw new LocalFault({key: 'profileBinding'});
  if (facts.numericErrors) throw new LocalFault({key: 'numericFields'});
  return facts.parsed.values;
}

export function editDraft(bound:Bound, draft:Record<string,Json>):Bound {
  return {...bound, draft, draftRevision: bound.draftRevision + 1, validation: null, touched: true};
}

export function newDraft(bound:Bound, presetName = ''):Bound {
  const preset = presetName ? bound.package.profiles[presetName] : undefined;
  return editDraft({...bound, selectedId: null, name: presetName, preset: presetName},
    preset ? structuredClone(preset.options) : defaultDraft(bound.package.schema));
}

export function selectProfile(bound:Bound, id:string):Bound {
  if (!id) return newDraft(bound);
  const profile = bound.profiles.find(item => item.id === id);
  if (!profile) return bound;
  return editDraft({...bound, selectedId: profile.id, name: profile.name, preset: ''}, structuredClone(profile.values));
}

export function workspaceRef(workspace:Pick<Workspace,'id'|'revision'>):WorkspaceRef {
  return {workspace_id: workspace.id, revision: workspace.revision};
}

// Labels are display names; equal display names stay distinguishable by the unique internal name.
export function workspaceLabel(workspace:Pick<Workspace,'id'|'internalName'|'displayName'>, all:Pick<Workspace,'id'|'internalName'|'displayName'>[]):string {
  const shared = all.some(other => other.id !== workspace.id && other.displayName === workspace.displayName);
  return shared ? `${workspace.displayName} · ${workspace.internalName}` : workspace.displayName;
}

export type OriginLabel = {kind:'open'; label:string} | {kind:'closed'; label:string} | {kind:'unknown'; label:string} | {kind:'application'; label:string};

// Attribution is honest about closed and unknown origins; it never assigns an event to a newer session.
export function originLabel(workspaceId:string|null, open:Workspace[], closed:ClosedWorkspace[], locale:Locale = 'en'):OriginLabel {
  const t = messages[locale].app;
  if (workspaceId === null) return {kind: 'application', label: t.application};
  const current = open.find(item => item.id === workspaceId);
  if (current) return {kind: 'open', label: workspaceLabel(current, open)};
  const retained = closed.find(item => item.id === workspaceId);
  if (retained) return {kind: 'closed', label: t.closedLabel(retained.label)};
  return {kind: 'unknown', label: t.unknownOrigin(workspaceId)};
}

export type LogScope = {kind:'workspace'; id:string} | {kind:'application'} | {kind:'all'};
export interface LogFilter {text:string; level:string}
export const LOG_LEVELS: readonly string[] = ['error', 'warn', 'info', 'debug', 'trace'];

export function inScope(entry:Pick<LogEntry,'workspace_id'>, scope:LogScope):boolean {
  if (scope.kind === 'all') return true;
  if (scope.kind === 'application') return entry.workspace_id === null;
  return entry.workspace_id === scope.id;
}

// Search covers only ordinary display fields; diagnostic fields and private records are never matched.
export function matchesFilter(entry:LogEntry, filter:LogFilter):boolean {
  if (filter.level && entry.level.toLowerCase() !== filter.level) return false;
  const needle = filter.text.trim().toLowerCase();
  if (!needle) return true;
  return [entry.code, entry.message, entry.source, entry.run ?? ''].some(field => field.toLowerCase().includes(needle));
}

export interface LogView {scoped:LogEntry[]; shown:LogEntry[]}
export function viewLogs(items:LogEntry[], scope:LogScope, filter:LogFilter):LogView {
  const scoped = items.filter(entry => inScope(entry, scope));
  return {scoped, shown: scoped.filter(entry => matchesFilter(entry, filter))};
}

export interface RetainedResult {ref:WorkspaceRef; view:ControllerView}

// Host-side retention is authoritative; a poll only refreshes what changed and drops closed owners.
export function ingestResults(current:Record<string,RetainedResult>, incoming:{workspace:WorkspaceRef; controller:ControllerView}[], open:Workspace[]):Record<string,RetainedResult> {
  const next:Record<string,RetainedResult> = {};
  let changed = false;
  for (const item of incoming) {
    if (!open.some(workspace => workspace.id === item.workspace.workspace_id)) continue;
    const previous = current[item.workspace.workspace_id];
    if (previous && previous.view.run === item.controller.run && previous.view.state === item.controller.state && previous.ref.revision === item.workspace.revision) {
      next[item.workspace.workspace_id] = previous;
    } else {
      next[item.workspace.workspace_id] = {ref: item.workspace, view: item.controller};
      changed = true;
    }
  }
  if (!changed && Object.keys(next).length === Object.keys(current).length) return current;
  return next;
}

// Unresolved failure state derives from retained outcomes, so no toast or view change can clear it.
export function needsAttention(view:ControllerView|null):boolean {
  if (!view || view.state !== 'terminal') return false;
  if (view.error) return true;
  const result = view.result;
  if (!result) return true;
  if (result.primary !== undefined && result.primary !== null) return true;
  if (result.forced === true) return true;
  const cleanup = result.cleanup;
  return !(cleanup !== null && typeof cleanup === 'object' && !Array.isArray(cleanup) && cleanup.clean === true);
}

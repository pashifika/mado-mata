import {defaultDraft} from './state.ts';
import type {ControllerView, Fault, Json, LogEntry, PackageInfo, Profile, Selection, WorkspaceRef} from './types.ts';

export const WORKSPACE_LIMIT = 8;
export const CLOSED_LIMIT = 16;
// The host caps a retained check descriptor; refuse longer paths inline instead of after a round trip.
export const DESCRIPTOR_LIMIT = 4096;
const BUSY_PHASES: Record<string, true> = {preparing: true, running: true, stopping: true};

// The three lifecycle phases during which the runner is reserved by one operation.
export function busy(state:string):boolean {
  return BUSY_PHASES[state] === true;
}

export type Page = 'run' | 'logs';

// Session-local UI state for one inspected package root. Nothing here is persisted.
export interface Workspace {
  id:string; revision:number; packagePath:string; package:PackageInfo;
  // Last readable catalog and its current listing fault, shared only after an authoritative read.
  profiles:Profile[]; profilesError:Fault|null;
  selectedId:string|null; name:string; preset:string; draft:Record<string,Json>;
  // Local edit counter: a validation or save that raced a later edit must not overwrite it.
  draftRevision:number; validation:Record<string,Json>|null;
  lane:string; scenario:string; descriptorPath:string; page:Page;
  error:Fault|null; notice:string; disclosedRun:string|null;
  logFilter:LogFilter;
  // True once the operator changed profile/draft state; a pristine default draft closes without confirmation.
  touched:boolean;
  // Label of the in-flight state-changing command owned by this workspace, if any.
  busy:string|null;
}

// Retained after close so diagnostics stay attributable without reopening the package.
export interface ClosedWorkspace {id:string; revision:number; label:string; result:ControllerView|null}

export interface Origin {id:string; revision:number; draftRevision?:number}

export type ProfileCatalog = Pick<Selection,'profiles'|'profiles_error'>;
export interface WorkspaceCommand {
  update:(workspace:Workspace) => Workspace;
  catalog?:ProfileCatalog;
}

// Execution configuration and navigation survive a reinspection; package-bound draft state does not.
export function freshWorkspace(selection:Selection, previous?:Workspace):Workspace {
  return {
    id: selection.workspace_id, revision: selection.revision, packagePath: selection.package_path, package: selection.package,
    profiles: selection.profiles, profilesError: selection.profiles_error,
    selectedId: null, name: '', preset: '', draft: defaultDraft(selection.package.schema),
    draftRevision: (previous?.draftRevision ?? 0) + 1, validation: null,
    lane: previous?.lane ?? 'controlled', scenario: previous?.scenario ?? 'workflow', descriptorPath: previous?.descriptorPath ?? '',
    page: previous?.page ?? 'run', error: null, notice: '', disclosedRun: null, logFilter: previous?.logFilter ?? {text: '', level: ''}, touched: false, busy: null,
  };
}

// Opening an already open root keeps its draft but takes the host's fresh saved-profile catalog; a new revision of a
// known id is a deliberate reset. Either way the catalog reaches every open workspace of the same package/schema.
// The host enforces the workspace limit; the UI only reflects it.
export function openWorkspace(list:Workspace[], selection:Selection):Workspace[] {
  const index = list.findIndex(item => item.id === selection.workspace_id);
  const existing = index < 0 ? undefined : list[index];
  let opened:Workspace;
  if (existing && existing.revision === selection.revision) {
    const reopened = {...existing, profilesError: selection.profiles_error, notice: 'This package root is already open; its draft is unchanged.'};
    opened = catalogKnown(selection.profiles_error) ? reconcileProfiles(reopened, selection.profiles) : reopened;
  } else {
    opened = {...freshWorkspace(selection, existing), notice: 'Package inspected. Start will revalidate its identity and capture current values.'};
  }
  const next = [...list];
  if (existing) next[index] = opened; else next.push(opened);
  return shareCatalog(next, selection.workspace_id, selection);
}

// The host lists the whole store or nothing: one malformed, oversized, or unsupported file fails the entire listing,
// which then arrives as `[]` plus a fault. That says nothing about the profiles themselves, so the last known
// catalog, selection, and draft stay until a later read succeeds. Only `ProfileRejected` is a real, partial catalog:
// the compatible profiles are listed and the rejected ones are named in the fault.
function catalogKnown(error:Fault|null):boolean {
  return error === null || error.category === 'ProfileRejected';
}

// Takes a fresh catalog without touching draft values. A selected profile that changed elsewhere leaves the local
// values as an explained draft, so Start never carries an identity the store no longer matches; the profile name
// follows a rename only while it was not edited locally, so a stale name cannot undo the rename on the next save.
function reconcileProfiles(workspace:Workspace, profiles:Profile[]):Workspace {
  const known = workspace.profiles;
  if (known === profiles || (known.length === profiles.length && JSON.stringify(known) === JSON.stringify(profiles))) return workspace;
  const previous = known.find(item => item.id === workspace.selectedId);
  if (!previous) return {...workspace, profiles};
  const current = profiles.find(item => item.id === previous.id);
  if (!current) {
    return {...workspace, profiles, selectedId: null, touched: true,
      notice: `Saved profile “${previous.name}” was deleted outside this workspace. Its values remain in this unsaved draft.`};
  }
  const renamed = previous.name !== current.name;
  const changed = JSON.stringify(previous.values) !== JSON.stringify(current.values);
  if (!renamed && !changed) return {...workspace, profiles};
  return {
    ...workspace, profiles, name: renamed && workspace.name === previous.name ? current.name : workspace.name,
    notice: changed
      ? `Saved profile “${current.name}” was updated outside this workspace. This draft keeps its own values: Update profile overwrites the saved ones; to load them instead, pick “Unsaved draft” and then the profile.`
      : `Saved profile was renamed to “${current.name}” outside this workspace. Draft values are unchanged.`,
  };
}

// Only an authoritative listing can replace sibling catalogs or clear their old listing faults.
// A failed read says nothing about stored profiles; keep every catalog and report it only at the origin.
export function shareCatalog(list:Workspace[], sourceId:string, catalog:ProfileCatalog):Workspace[] {
  const source = list.find(item => item.id === sourceId);
  if (!source) return list;
  if (!catalogKnown(catalog.profiles_error)) {
    return updateWorkspace(list, sourceId, item => ({...item, profilesError: catalog.profiles_error}));
  }
  let changed = false;
  const next = list.map(item => {
    if (item.package.package_id !== source.package.package_id || item.package.schema_identity !== source.package.schema_identity) return item;
    const reconciled = reconcileProfiles(item, catalog.profiles);
    const updated = reconciled.profilesError === catalog.profiles_error ? reconciled : {...reconciled, profilesError: catalog.profiles_error};
    if (updated !== item) changed = true;
    return updated;
  });
  return changed ? next : list;
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
  if (origin.draftRevision !== undefined && origin.draftRevision !== current.draftRevision) return list;
  const next = [...list];
  next[index] = update(current);
  return next;
}

// Validation and failed commands carry no fresh listing and cannot republish a stale cache.
export function applyCommand(list:Workspace[], origin:Origin, command:WorkspaceCommand):Workspace[] {
  const next = applyIfCurrent(list, origin, command.update);
  return next === list || !command.catalog ? next : shareCatalog(next, origin.id, command.catalog);
}

export function updateWorkspace(list:Workspace[], id:string, update:(workspace:Workspace) => Workspace):Workspace[] {
  const index = list.findIndex(item => item.id === id);
  if (index < 0) return list;
  const next = [...list];
  next[index] = update(list[index]);
  return next;
}

export function editDraft(workspace:Workspace, draft:Record<string,Json>):Workspace {
  return {...workspace, draft, draftRevision: workspace.draftRevision + 1, validation: null, notice: '', touched: true};
}

export function newDraft(workspace:Workspace, presetName = ''):Workspace {
  const preset = presetName ? workspace.package.profiles[presetName] : undefined;
  return editDraft({...workspace, selectedId: null, name: presetName, preset: presetName},
    preset ? structuredClone(preset.options) : defaultDraft(workspace.package.schema));
}

export function selectProfile(workspace:Workspace, id:string):Workspace {
  if (!id) return newDraft(workspace);
  const profile = workspace.profiles.find(item => item.id === id);
  if (!profile) return workspace;
  return editDraft({...workspace, selectedId: profile.id, name: profile.name, preset: ''}, structuredClone(profile.values));
}

export function workspaceRef(workspace:Pick<Workspace,'id'|'revision'>):WorkspaceRef {
  return {workspace_id: workspace.id, revision: workspace.revision};
}

// Same package IDs from different roots stay distinguishable by their last path segment.
export function workspaceLabel(workspace:Pick<Workspace,'id'|'packagePath'|'package'>, all:Pick<Workspace,'id'|'packagePath'|'package'>[]):string {
  const shared = all.some(other => other.id !== workspace.id && other.package.package_id === workspace.package.package_id);
  if (!shared) return workspace.package.package_id;
  const segment = workspace.packagePath.split(/[\\/]+/).filter(Boolean).pop() ?? workspace.packagePath;
  return `${workspace.package.package_id} · ${segment}`;
}

export type OriginLabel = {kind:'open'; label:string} | {kind:'closed'; label:string} | {kind:'unknown'; label:string} | {kind:'application'; label:string};

// Attribution is honest about closed and unknown origins; it never assigns an event to a newer tab.
export function originLabel(workspaceId:string|null, open:Workspace[], closed:ClosedWorkspace[]):OriginLabel {
  if (workspaceId === null) return {kind: 'application', label: 'Application'};
  const current = open.find(item => item.id === workspaceId);
  if (current) return {kind: 'open', label: workspaceLabel(current, open)};
  const retained = closed.find(item => item.id === workspaceId);
  if (retained) return {kind: 'closed', label: `${retained.label} · closed`};
  return {kind: 'unknown', label: `${workspaceId} · closed, no retained detail`};
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

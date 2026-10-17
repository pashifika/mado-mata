import type {AuthoringController, ExportTicket} from './authoring-controller.ts';
import {draftList, sameAuthoringRef} from './authoring.ts';
import type {AuthoringSession, SourceLocation} from './authoring.ts';
import {defaultDraft, record} from './state.ts';
import {clearNativeApproval} from './workspace.ts';
import type {Bound} from './workspace.ts';
import {readTarget, targetReadFailed, targetState} from './target.ts';
import {mutatedRecovery, recoveryState, sameRecoveryContext, upsertOutcomes} from './recovery.ts';
import type {RecoveryState, RecoveryTicket} from './recovery.ts';
import type {AuthoringTrialContext, Fault, FunctionCandidate, Json, ProfileRecoveryOutcome, RecoveryMutation} from './types.ts';

export interface EditTrialState {
  context:AuthoringTrialContext; bound:Bound; unit:'package'|'function'; selected:string|null; arguments:string;
  profileChoice:string; review:ExportTicket|null;
  recovery:RecoveryState|null; recoveryOutcomes:ProfileRecoveryOutcome[];
}

export function functionKey(candidate:Pick<FunctionCandidate,'module'|'function'>):string {
  return JSON.stringify([candidate.module, candidate.function]);
}

export function trialContext(context:AuthoringTrialContext, session:AuthoringSession, internalName:string, previous:EditTrialState|null):EditTrialState|null {
  if (!sameAuthoringRef(context.owner, session.owner) || context.revision !== session.revision) return null;
  const same = previous !== null && sameAuthoringRef(previous.context.owner, context.owner);
  const selection = {...context.owner.workspace, internal_name:internalName, display_name:'', package:context.package,
    profiles:context.profiles, profiles_error:context.profiles_error, package_path:session.packagePath,
    source:{kind:'directory' as const, path:session.packagePath}};
  const old = same ? previous.bound : null;
  const base:Bound = old ? {...clearNativeApproval(old), package:context.package, profiles:context.profiles, profilesError:context.profiles_error}
    : {packagePath:session.packagePath, package:context.package, profiles:context.profiles, profilesError:context.profiles_error,
      selectedId:null, name:'', preset:'', draft:defaultDraft(context.package.schema), draftRevision:0,
      lane:'controlled', scenario:'workflow', descriptorPath:'', touched:false, target:targetState(selection),
      native:{operation:'', postcondition:'', workflowSeconds:null, capture:false, input:false, launch:false, recovery:false, key:null}};
  const target = old && old.target.context.declaration_identity === context.package.target_identity ? old.target : targetState(selection);
  base.target = context.target ? readTarget(target, context.target)
    : context.target_error ? targetReadFailed(target, context.target_error) : target;
  const selected = same && previous.selected !== null && context.functions.some(candidate => functionKey(candidate) === previous.selected)
    ? previous.selected : null;
  const reviewCurrent = same && previous.context.revision === context.revision
    && previous.context.package.inventory_identity === context.package.inventory_identity;
  if (old && reviewCurrent) base.native = old.native;
  let profileChoice = same ? previous.profileChoice : '';
  if (profileChoice.startsWith('profile:') && !context.profiles.some(profile => profile.id === profileChoice.slice(8))) profileChoice = '';
  if (profileChoice.startsWith('preset:') && !Object.hasOwn(context.package.profiles, profileChoice.slice(7))) profileChoice = '';
  return {context, bound:base, unit:same ? previous.unit : 'package', selected, arguments:same ? previous.arguments : '[]',
    profileChoice, review:reviewCurrent ? previous.review : null,
    recovery:context.recovery ? recoveryState(context.recovery, same ? previous.recovery : null) : null,
    recoveryOutcomes:same ? previous.recoveryOutcomes : []};
}

export function applyTrialRecovery(state:EditTrialState, ticket:RecoveryTicket, mutation:RecoveryMutation):EditTrialState {
  const saved = mutation.saved;
  const recoveryOutcomes = saved ? upsertOutcomes(state.recoveryOutcomes,
    [{profile_id:saved.id, name:saved.name, status:'saved', issue:null}]) : state.recoveryOutcomes;
  if (!state.recovery || !sameRecoveryContext(state.recovery.view.context, ticket.context)) return {...state, recoveryOutcomes};
  let bound = clearNativeApproval(state.bound);
  if (mutation.catalog) bound = {...bound, profiles:mutation.catalog.profiles, profilesError:mutation.catalog.profiles_error};
  else if (mutation.refresh_error) bound = {...bound, profiles:[], profilesError:mutation.refresh_error};
  else if (saved && saved.package_id === bound.package.package_id && saved.schema_identity === bound.package.schema_identity) {
    bound = {...bound, profiles:[...bound.profiles.filter(profile => profile.id !== saved.id), saved].sort((a, b) => a.name.localeCompare(b.name))};
  }
  const profileChoice = state.profileChoice.startsWith('profile:') && !bound.profiles.some(profile => profile.id === state.profileChoice.slice(8)) ? '' : state.profileChoice;
  return {...state, bound, profileChoice, review:null, recovery:mutatedRecovery(state.recovery, ticket, mutation), recoveryOutcomes};
}

export type ArgumentError = 'json'|'array'|'bytes'|'depth'|'count'|'required';
export function trialArguments(source:string, required = 0):{values:Json[]; error:null}|{values:null; error:ArgumentError} {
  if (new TextEncoder().encode(source).length > 65_536) return {values:null, error:'bytes'};
  let value:Json;
  try {value = JSON.parse(source) as Json;} catch {return {values:null, error:'json'};}
  if (!Array.isArray(value)) return {values:null, error:'array'};
  if (value.length > 32) return {values:null, error:'count'};
  const pending = value.map(item => ({value:item, depth:1}));
  while (pending.length) {
    const item = pending.pop()!;
    if (item.depth > 32) return {values:null, error:'depth'};
    if (item.value !== null && typeof item.value === 'object') {
      for (const child of Object.values(item.value)) pending.push({value:child, depth:item.depth + 1});
    }
  }
  return value.length < required ? {values:null, error:'required'} : {values:value, error:null};
}

export type TrialSourceBlock = 'pending'|'refresh'|'composition'|'missing'|'dirty';
export function trialSourceBlock(session:AuthoringSession, dirty:boolean, reading = false):TrialSourceBlock|null {
  if (!reading && session.pending !== null) return 'pending';
  if (session.refreshRequired || session.conflict || draftList(session).some(draft => draft.diskChanged)) return 'refresh';
  if (draftList(session).some(draft => draft.composing !== null)) return 'composition';
  if (draftList(session).some(draft => draft.missing)) return 'missing';
  return dirty ? 'dirty' : null;
}

export function trialSourceTicket(controller:AuthoringController):ExportTicket|null {
  const session = controller.current();
  return session && trialSourceBlock(session, false) === null ? controller.exportTicket() : null;
}

export function currentTrialSource(controller:AuthoringController, ticket:ExportTicket):boolean {
  const session = controller.current();
  return session !== null && trialSourceBlock(session, false, true) === null && controller.exportCurrent(ticket);
}

// Match saved validation's compiler envelope, retaining only source facts for navigation.
export function trialDiagnostics(diagnostic:Fault):Fault[] {
  const context = record(diagnostic.context);
  if (diagnostic.category !== 'TypeScript' || !Array.isArray(context.diagnostics)) return [];
  const diagnostics:Fault[] = [];
  for (const value of context.diagnostics.slice(0, 100)) {
    const item = record(value);
    if (typeof item.message !== 'string') continue;
    const location = sourceLocation({module:item.module ?? null, line:item.line ?? null, column:item.column ?? null});
    diagnostics.push({category:'TypeScript', message:item.message.length > 4096 ? `${item.message.slice(0, 4096)}…` : item.message,
      context:{path:location?.path ?? null, line:location?.line ?? null, column:location?.column ?? null,
        code:typeof item.code === 'number' && Number.isSafeInteger(item.code) ? item.code : null}});
  }
  return diagnostics;
}

function sourceLocation(location:Record<string,Json>):SourceLocation|null {
  const path = typeof location.path === 'string' ? location.path : location.module;
  if (typeof path !== 'string' || !path || typeof location.line !== 'number' || !Number.isSafeInteger(location.line) || location.line < 1) return null;
  const column = location.column ?? 1;
  if (typeof column !== 'number' || !Number.isSafeInteger(column) || column < 1) return null;
  return {path, line:location.line, column};
}

export function trialDiagnosticLocation(diagnostic:Fault):SourceLocation|null {
  const context = record(diagnostic.context);
  if (diagnostic.category === 'TypeScript' && Array.isArray(context.diagnostics)) {
    for (const item of trialDiagnostics(diagnostic)) {
      const location = sourceLocation(record(item.context));
      if (location) return location;
    }
    return null;
  }
  if (context.typescript !== undefined) {
    const typescript = record(context.typescript);
    const frames = Array.isArray(typescript.frames) ? typescript.frames : [];
    for (const frame of frames) {
      const location = sourceLocation(record(record(frame).original));
      if (location) return location;
    }
    return null;
  }
  return sourceLocation(context);
}

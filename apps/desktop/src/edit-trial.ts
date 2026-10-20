import type {AuthoringController, ExportTicket} from './authoring-controller.ts';
import {draftList, sameAuthoringRef} from './authoring.ts';
import type {AuthoringSession, SourceLocation} from './authoring.ts';
import {record} from './state.ts';
import {clearNativeApproval, deriveBound, newDraft} from './workspace.ts';
import type {Bound} from './workspace.ts';
import {readTarget, targetReadFailed, targetState} from './target.ts';
import {mutatedRecovery, recoveryState, sameRecoveryContext, upsertOutcomes} from './recovery.ts';
import type {RecoveryState, RecoveryTicket} from './recovery.ts';
import type {AuthoringRef, AuthoringTrialContext, Fault, FunctionCandidate, Json, NativeCapability, OcrEnvironment, ProfileRecoveryOutcome, RecoveryMutation, StartRequest} from './types.ts';

// Execution setup only: what an explicit Run executes against the latest saved Script, never a source approval.
export interface EditTrialState {
  context:AuthoringTrialContext; bound:Bound; unit:'package'|'function'; selected:string|null; arguments:string;
  profileChoice:string;
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
  const preset = Object.hasOwn(context.package.profiles, 'default') ? 'default' : '';
  // Saved-source revisions keep the whole setup, including optional launch/recovery choices; their key covers target,
  // values, environment and limits, never the Script revision.
  const base:Bound = old ? {...old, package:context.package, profiles:context.profiles, profilesError:context.profiles_error}
    : {...newDraft({packagePath:session.packagePath, package:context.package, profiles:context.profiles, profilesError:context.profiles_error,
      selectedId:null, name:'', preset:'', draft:{}, draftRevision:0,
      lane:context.package.target === null ? 'controlled' : 'native', scenario:'workflow', descriptorPath:'', touched:false, target:targetState(selection),
      native:{operation:'', postcondition:'', workflowSeconds:null, capture:false, input:false, launch:false, recovery:false, key:null}}, preset), touched:false};
  const target = old && old.target.context.declaration_identity === context.package.target_identity ? old.target : targetState(selection);
  base.target = context.target ? readTarget(target, context.target)
    : context.target_error ? targetReadFailed(target, context.target_error) : target;
  const selected = same && previous.selected !== null && context.functions.some(candidate => functionKey(candidate) === previous.selected)
    ? previous.selected : null;
  let profileChoice = same ? previous.profileChoice : preset ? 'preset:default' : 'values';
  if (profileChoice.startsWith('profile:') && !context.profiles.some(profile => profile.id === profileChoice.slice(8))) profileChoice = '';
  if (profileChoice.startsWith('preset:') && !Object.hasOwn(context.package.profiles, profileChoice.slice(7))) profileChoice = '';
  return {context, bound:base, unit:same ? previous.unit : 'package', selected, arguments:same ? previous.arguments : '[]',
    profileChoice,
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
  return {...state, bound, profileChoice, recovery:mutatedRecovery(state.recovery, ticket, mutation), recoveryOutcomes};
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

// The request a ready setup describes for its own saved source; it carries no capture/input authority.
function trialRequest(state:EditTrialState, environment:OcrEnvironment|null, capability:NativeCapability|null):{request:StartRequest; profileName:string}|null {
  const bound = state.bound;
  const facts = deriveBound(bound, environment, 'en', capability, 'script');
  const candidate = state.context.functions.find(item => functionKey(item) === state.selected);
  const args = trialArguments(state.arguments, candidate?.required_arguments);
  if (!state.profileChoice || !facts.bound || facts.numericErrors || facts.startBlock || facts.descriptorError
    || (state.unit === 'function' && (!candidate || args.error))) return null;
  const native = bound.lane === 'native' ? facts.native?.intent ?? null : null;
  if (bound.lane === 'native' && native === null) return null;
  const profile = facts.selectedProfile;
  return {profileName:profile && !facts.valuesDirty ? profile.name : bound.name, request:{
    package_path:bound.packagePath, inventory_identity:state.context.package.inventory_identity,
    package_id:profile?.package_id ?? bound.package.package_id, schema_identity:profile?.schema_identity ?? bound.package.schema_identity,
    profile_id:profile && !facts.valuesDirty ? profile.id : 'draft', values:facts.parsed.values,
    lane:bound.lane, scenario:'workflow', replay_descriptor_path:bound.lane === 'replay' ? bound.descriptorPath.trim() || null : null,
    native_intent:native && state.unit === 'function' ? {...native, launch_approved:false, max_exit_recoveries:0} : native,
    function:state.unit === 'function' && candidate && args.values ? {module:candidate.module, function:candidate.function, arguments:args.values} : null,
  }};
}

// What one explicit Run selected. Saved source and host facts are reread before dispatch; the setup itself is not.
export interface TrialRunIntent {
  owner:AuthoringRef; unit:EditTrialState['unit']; selected:string|null; lane:string;
  // The saved Native target shown when Run was pressed: a reread may refuse it, never replace it.
  target:{id:string; revision:number}|null;
  startup:{launch:boolean; recoveries:number};
}

export function trialRunIntent(state:EditTrialState, environment:OcrEnvironment|null, capability:NativeCapability|null):TrialRunIntent {
  const saved = state.bound.target.view?.record;
  const native = state.unit === 'package' && state.bound.lane === 'native'
    ? deriveBound(state.bound, environment, 'en', capability, 'script').native?.intent : null;
  return {owner:state.context.owner, unit:state.unit, selected:state.unit === 'function' ? state.selected : null, lane:state.bound.lane,
    target:saved?.binding ? {id:saved.binding.id, revision:saved.revision} : null,
    startup:{launch:native?.launch_approved ?? false, recoveries:native?.max_exit_recoveries ?? 0}};
}

export type TrialRunRefusal = 'changed'|'selectAgain'|'setupIncomplete';

// The final request for one explicit Run, after the latest saved source and setup were reread. That Run is this one
// request's capture/input authority: nothing persists it, and selecting, saving or closing setup never grants it.
export function trialRunRequest(intent:TrialRunIntent, state:EditTrialState, source:ExportTicket, environment:OcrEnvironment|null,
  capability:NativeCapability|null):{request:StartRequest; profileName:string}|TrialRunRefusal {
  if (!sameAuthoringRef(intent.owner, state.context.owner) || !sameAuthoringRef(state.context.owner, source.owner)
    || state.context.revision !== source.revision || intent.unit !== state.unit || intent.lane !== state.bound.lane) return 'changed';
  if (state.unit === 'function' && state.selected === null) return 'selectAgain';
  if (state.unit === 'function' && state.selected !== intent.selected) return 'changed';
  const prepared = trialRequest(state, environment, capability);
  if (!prepared) return 'setupIncomplete';
  const native = prepared.request.native_intent;
  if (!native) return prepared;
  if (!intent.target || native.target_binding_id !== intent.target.id || native.target_revision !== intent.target.revision) return 'changed';
  if (native.launch_approved !== intent.startup.launch || native.max_exit_recoveries !== intent.startup.recoveries) return 'changed';
  return {...prepared, request:{...prepared.request, native_intent:{...native, capture_approved:true, input_approved:true}}};
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

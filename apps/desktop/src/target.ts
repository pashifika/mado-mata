import type {Fault, Selection, TargetApplicationResponse, TargetCheck, TargetCheckResponse, TargetConfiguration, TargetContext, TargetDeclaration, TargetExpectation, TargetInputPolicy, TargetLocation, TargetResolution, TargetSaveResponse, TargetView} from './types.ts';

const encoder = new TextEncoder();

export interface TargetDraft {
  gameKind:TargetLocation['kind']|''; gamePath:string;
  separateLauncher:boolean; launcherKind:TargetLocation['kind']|''; launcherPath:string;
  arguments:string[]; workingDirectory:string; windowTitle:string;
  route:TargetInputPolicy['route']|''; focus:TargetInputPolicy['focus']|'';
  pointerMode:Exclude<TargetInputPolicy['pointer_mode'],null>|''; clickHold:string;
}
export type TargetOperation = 'read'|'check'|'save'|'remove';
export interface TargetTicket {
  context:TargetContext; draftRevision:number; draftKey:string; expected:TargetExpectation;
}
export interface TargetObservation {ticket:TargetTicket; check:TargetCheck}
export interface TargetReview {ticket:TargetTicket; previous:TargetResolution|null; resolution:TargetResolution}
export type TargetPickerField = 'game'|'launcher';
export interface TargetPickerTicket {ticket:TargetTicket; field:TargetPickerField}
export interface RunningApplicationState {
  pending:{ticket:TargetTicket; requestId:string}|null;
  result:{ticket:TargetTicket; response:TargetApplicationResponse}|null;
  issue:{ticket:TargetTicket; fault:Fault}|null;
  cancelled:boolean;
}
export interface TargetState {
  context:TargetContext; declaration:TargetDeclaration|null; view:TargetView|null; draft:TargetDraft; draftRevision:number;
  operation:TargetOperation|null; loaded:boolean; readError:Fault|null; refreshRequired:boolean; reconcile:boolean;
  issue:{fault:Fault; ticket:TargetTicket}|null; observation:TargetObservation|null; review:TargetReview|null;
  application:RunningApplicationState; picker:TargetPickerTicket|null; pickerIssue:Fault|null;
  persisted:'saved'|'removed'|null;
}
const idleApplication:RunningApplicationState = {pending:null, result:null, issue:null, cancelled:false};

function blankDraft(declaration:TargetDeclaration|null):TargetDraft {
  return {gameKind:'', gamePath:'', separateLauncher:false, launcherKind:'', launcherPath:'', arguments:[],
    workingDirectory:'', windowTitle:declaration?.window_title ?? '', route:'', focus:'', pointerMode:'', clickHold:''};
}

function configurationDraft(value:TargetConfiguration):TargetDraft {
  return {gameKind:value.game.kind, gamePath:value.game.path, separateLauncher:value.launcher !== null,
    launcherKind:value.launcher?.kind ?? '', launcherPath:value.launcher?.path ?? '', arguments:[...value.arguments],
    workingDirectory:value.working_directory ?? '', windowTitle:value.window_title, route:value.input.route,
    focus:value.input.focus, pointerMode:value.input.pointer_mode ?? '', clickHold:String(value.input.click_hold_ms)};
}

export function targetState(selection:Selection):TargetState {
  return {
    context:{workspace:{workspace_id:selection.workspace_id, revision:selection.revision}, internal_name:selection.internal_name,
      package_id:selection.package.package_id, declaration_identity:selection.package.target_identity},
    declaration:selection.package.target, view:null, draft:blankDraft(selection.package.target), draftRevision:0,
    operation:null, loaded:false, readError:null, refreshRequired:false, reconcile:false,
    issue:null, observation:null, review:null, application:idleApplication,
    picker:null, pickerIssue:null, persisted:null,
  };
}

function sameContext(a:TargetContext, b:TargetContext):boolean {
  return a.workspace.workspace_id === b.workspace.workspace_id && a.workspace.revision === b.workspace.revision
    && a.internal_name === b.internal_name && a.package_id === b.package_id && a.declaration_identity === b.declaration_identity;
}

function savedDraft(state:TargetState):TargetDraft {
  const binding = state.view?.compatible ? state.view.record.binding : null;
  return binding ? configurationDraft(binding.configuration) : blankDraft(state.declaration);
}

export function targetDirty(state:TargetState):boolean {
  const configuration = readTargetDraft(state.draft).configuration;
  const draft = configuration && state.view?.compatible && state.view.record.binding ? configurationDraft(configuration) : state.draft;
  return JSON.stringify(draft) !== JSON.stringify(savedDraft(state));
}

export function targetExpectation(state:TargetState):TargetExpectation|null {
  return state.view && state.loaded && !state.readError && !state.refreshRequired && !state.reconcile
    ? {revision:state.view.record.revision, binding_id:state.view.record.binding?.id ?? null} : null;
}

export function targetTicket(state:TargetState):TargetTicket|null {
  const expected = targetExpectation(state);
  return expected ? {context:state.context, expected, draftRevision:state.draftRevision, draftKey:JSON.stringify(state.draft)} : null;
}

export function currentTargetDraft(state:TargetState, ticket:TargetTicket):boolean {
  return sameContext(state.context, ticket.context) && state.draftRevision === ticket.draftRevision
    && JSON.stringify(state.draft) === ticket.draftKey;
}

function currentRecord(state:TargetState, ticket:TargetTicket):boolean {
  return sameContext(state.context, ticket.context) && state.view?.record.revision === ticket.expected.revision
    && (state.view.record.binding?.id ?? null) === ticket.expected.binding_id;
}
export function eligibleRunningApplication(state:TargetState):TargetTicket|null {
  const ticket = targetTicket(state);
  return ticket && state.operation === null && state.application.pending === null && state.picker === null
    && state.review === null && !targetDirty(state) && state.view?.compatible
    && state.view.record.binding?.configuration.game.kind === 'bundle' ? ticket : null;
}

export function beginRunningApplication(state:TargetState, ticket:TargetTicket, requestId:string):TargetState {
  if (!eligibleRunningApplication(state) || !currentTargetDraft(state, ticket) || !currentRecord(state, ticket)) return state;
  return {...state, application:{pending:{ticket, requestId}, result:null, issue:null, cancelled:false}};
}

export function completeRunningApplication(state:TargetState, ticket:TargetTicket, response:TargetApplicationResponse):TargetState {
  if (!state.application.pending || state.application.pending.ticket !== ticket
    || state.application.pending.requestId !== response.request_id || !currentTargetDraft(state, ticket)
    || !currentRecord(state, ticket) || !sameContext(ticket.context, response.context)
    || response.revision !== ticket.expected.revision || response.binding_id !== ticket.expected.binding_id) return state;
  return {...state, application:{pending:null, result:{ticket, response}, issue:null, cancelled:false}};
}

export function failRunningApplication(state:TargetState, ticket:TargetTicket, requestId:string, error:Fault):TargetState {
  if (!state.application.pending || state.application.pending.ticket !== ticket
    || state.application.pending.requestId !== requestId || !currentTargetDraft(state, ticket)
    || !currentRecord(state, ticket)) return state;
  const failed = {...state, application:{pending:null, result:null, issue:{ticket, fault:error}, cancelled:false}};
  return error.category === 'TargetResolutionChanged' || error.category === 'TargetConflict'
    ? {...targetFailed(failed, ticket, error), application:failed.application} : failed;
}

export function invalidateRunningApplication(state:TargetState):TargetState {
  return {...state, application:idleApplication};
}

export function cancelRunningApplication(state:TargetState, requestId:string):TargetState {
  return state.application.pending?.requestId === requestId
    ? {...state, application:{pending:null, result:null, issue:null, cancelled:true}} : state;
}

export function beginApplicationPicker(state:TargetState, field:TargetPickerField, issued?:TargetPickerTicket):TargetState {
  const ticket = issued?.ticket ?? targetTicket(state);
  if (!ticket || !state.declaration || state.operation !== null || state.picker !== null
    || !currentTargetDraft(state, ticket) || !currentRecord(state, ticket)
    || (field === 'launcher' && !state.draft.separateLauncher)) return state;
  return {...state, picker:issued ?? {ticket, field}, pickerIssue:null};
}

export function completeApplicationPicker(state:TargetState, picker:TargetPickerTicket, path:string|null, error:Fault|null = null):TargetState {
  if (state.picker !== picker || !currentTargetDraft(state, picker.ticket) || !currentRecord(state, picker.ticket)) return state;
  const settled = {...state, picker:null, pickerIssue:error};
  if (path === null || error !== null) return settled;
  return picker.field === 'game'
    ? editTarget(settled, {...state.draft, gameKind:'bundle', gamePath:path})
    : editTarget(settled, {...state.draft, launcherKind:'bundle', launcherPath:path});
}

export function invalidateApplicationPicker(state:TargetState):TargetState {
  return {...state, picker:null, pickerIssue:null};
}

// Keep a completed mutation visible while its refresh is owed; later edits retire settled notices.
export function editTarget(state:TargetState, draft:TargetDraft):TargetState {
  return {...state, draft, draftRevision:state.draftRevision + 1, observation:null, issue:null, review:null,
    application:idleApplication, picker:null, pickerIssue:null,
    persisted:state.refreshRequired ? state.persisted : null};
}

export function discardTarget(state:TargetState):TargetState {
  return editTarget(state, savedDraft(state));
}

// A mutation's own refresh never passes through here (App.targetCommand reads inline), so a read beginning while a refresh
// is still owed is the recovery Reload and keeps the completed mutation's notice; any other Reload retires it.
export function beginTarget(state:TargetState, operation:TargetOperation):TargetState {
  return {...state, operation, application:idleApplication, picker:null, pickerIssue:null,
    ...(operation === 'read'
      ? {readError:null, persisted:state.refreshRequired ? state.persisted : null} : {issue:null, persisted:null})};
}

// Reads refresh only saved truth. The first read may initialize an untouched form; later reads never replace edits.
export function readTarget(state:TargetState, view:TargetView):TargetState {
  if (!sameContext(state.context, view.context) || view.record.internal_name !== state.context.internal_name
    || view.record.package_id !== state.context.package_id) return state;
  const next = {...state, view, loaded:true, readError:null, refreshRequired:false, reconcile:false,
    application:idleApplication, picker:null, pickerIssue:null};
  const changed = state.view !== null && (state.view.record.revision !== view.record.revision
    || state.view.record.binding?.id !== view.record.binding?.id);
  return {...next, draft:state.view === null && state.draftRevision === 0 ? savedDraft(next) : state.draft,
    issue:state.issue?.fault.category === 'TargetConflict' ? null : state.issue,
    ...(changed ? {observation:null, review:null} : {})};
}

export function targetReadFailed(state:TargetState, error:Fault):TargetState {
  return {...state, loaded:true, readError:error, refreshRequired:true,
    application:idleApplication, picker:null, pickerIssue:null};
}

export function checkedTarget(state:TargetState, ticket:TargetTicket, response:TargetCheckResponse):TargetState {
  if (!currentRecord(state, ticket) || !sameContext(ticket.context, response.context)
    || response.revision !== ticket.expected.revision || response.binding_id !== ticket.expected.binding_id) return state;
  const observation = {ticket, check:response.check};
  return {...state, issue:null, observation,
    review:response.check.resolution_changed ? {ticket, previous:response.check.previous_resolution, resolution:response.check.resolution} : null};
}

export function savedTarget(state:TargetState, ticket:TargetTicket, response:TargetSaveResponse):TargetState {
  if (!currentRecord(state, ticket) || !sameContext(ticket.context, response.view.context)
    || response.view.record.internal_name !== state.context.internal_name || response.view.record.package_id !== state.context.package_id) return state;
  return {...state, view:response.view, refreshRequired:true, persisted:'saved', issue:null, review:null,
    application:idleApplication, picker:null, pickerIssue:null,
    observation:{ticket, check:response.check}};
}

export function removedTarget(state:TargetState, ticket:TargetTicket, view:TargetView):TargetState {
  if (!currentRecord(state, ticket) || !sameContext(ticket.context, view.context)
    || view.record.internal_name !== state.context.internal_name || view.record.package_id !== state.context.package_id) return state;
  // Removal changes saved truth only; Discard remains the explicit way to clear the local draft.
  return {...state, view, refreshRequired:true, persisted:'removed', issue:null, review:null, observation:null,
    application:idleApplication, picker:null, pickerIssue:null};
}

// An attributed usable changed-resolution result is the review-needed state, not a fault; a malformed or missing
// resolution context stays a real failure.
export function targetFailed(state:TargetState, ticket:TargetTicket, error:Fault):TargetState {
  if (!sameContext(state.context, ticket.context)) return state;
  const context = error.context !== null && typeof error.context === 'object' && !Array.isArray(error.context) ? error.context : null;
  const resolution = context?.resolution;
  const review = error.category === 'TargetResolutionChanged' && resolution !== null && typeof resolution === 'object' && !Array.isArray(resolution)
    ? {ticket, previous:(context?.previous_resolution ?? null) as unknown as TargetResolution|null, resolution:resolution as unknown as TargetResolution} : null;
  return {...state, issue:review ? null : {fault:error, ticket}, observation:null, review,
    reconcile:state.reconcile || error.category === 'TargetConflict'};
}

export type TargetField = keyof TargetDraft;
export type TargetFieldError = 'required'|'path'|'text'|'arguments'|'hold'|'policy';
export function readTargetDraft(draft:TargetDraft):{configuration:TargetConfiguration|null; errors:Partial<Record<TargetField,TargetFieldError>>} {
  const errors:Partial<Record<TargetField,TargetFieldError>> = {};
  const bytes = (value:string) => encoder.encode(value).length;
  const controls = /\p{Cc}/u;
  const validPath = (value:string) => value.startsWith('/') && bytes(value) <= 4096 && !controls.test(value);
  if (!draft.gameKind) errors.gameKind = 'required';
  if (!validPath(draft.gamePath)) errors.gamePath = 'path';
  if (draft.separateLauncher) {
    if (!draft.launcherKind) errors.launcherKind = 'required';
    if (!validPath(draft.launcherPath)) errors.launcherPath = 'path';
  }
  if (draft.workingDirectory !== '' && !validPath(draft.workingDirectory)) errors.workingDirectory = 'path';
  if (draft.windowTitle === '' || bytes(draft.windowTitle) > 512 || controls.test(draft.windowTitle)) errors.windowTitle = 'text';
  if (draft.arguments.length > 32 || draft.arguments.some(arg => bytes(arg) > 1024 || controls.test(arg))
    || draft.arguments.reduce((sum, arg) => sum + bytes(arg), 0) > 8192) errors.arguments = 'arguments';
  if (!draft.route) errors.route = 'required';
  if (!draft.focus) errors.focus = 'required';
  if (draft.route === 'process_directed' && !draft.pointerMode) errors.pointerMode = 'required';
  if (draft.route === 'system' && draft.focus !== 'require_focused') errors.focus = 'policy';
  if (draft.route === 'process_directed' && draft.pointerMode === 'appkit_background' && draft.focus !== 'preserve') errors.focus = 'policy';
  if (!/^\d+$/.test(draft.clickHold) || Number(draft.clickHold) > 1000) errors.clickHold = 'hold';
  if (Object.keys(errors).length || !draft.gameKind || !draft.route || !draft.focus || (draft.separateLauncher && !draft.launcherKind)) return {configuration:null, errors};
  return {errors, configuration:{platform:'macos', game:{kind:draft.gameKind, path:draft.gamePath},
    launcher:draft.separateLauncher ? {kind:draft.launcherKind as TargetLocation['kind'], path:draft.launcherPath} : null,
    arguments:[...draft.arguments], working_directory:draft.workingDirectory === '' ? null : draft.workingDirectory,
    window_title:draft.windowTitle, input:{route:draft.route, focus:draft.focus,
      pointer_mode:draft.route === 'system' ? null : draft.pointerMode as Exclude<TargetInputPolicy['pointer_mode'],null>, click_hold_ms:Number(draft.clickHold)}}};
}

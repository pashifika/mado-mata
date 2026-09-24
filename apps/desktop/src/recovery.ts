import type {Locale} from './i18n.ts';
import {optionPath, readDraft} from './state.ts';
import type {Fault, Json, ProfileRecoveryOutcome, RecoveryMutation, RecoveryRef, RecoveryView, Schema} from './types.ts';

// Identifies the exact draft a Save/Reset checked: context token, profile and local edit counter.
export interface RecoveryTicket {context:RecoveryRef; profileId:string; draftRevision:number}

// A value path at which the loaded profile holds a JSON string under a number/integer schema. The editor represents
// numeric text as strings, so without this record the stored string would parse as if the operator had typed it.
export interface StoredText {segments:(string|number)[]; path:string; text:string}
export type RecoveryEdit = {kind:'replace'; path:string}
  | {kind:'move'; path:string; index:number; other:number}
  | {kind:'remove'; path:string; index:number};

// The operator's repair of one rejected profile. The draft is replaced only by an explicit selection, a still-current
// Reset response, or the operator's own edits; a host view never overwrites it.
export interface RecoveryState {
  view:RecoveryView;
  selectedId:string|null; draft:Record<string,Json>; draftRevision:number; touched:boolean;
  // Stored type mismatches in the draft, shown for deliberate replacement and never coerced; an edit that replaces one
  // retires it, so ordinary numeric text typed afterwards parses as usual.
  storedText:StoredText[];
  // Failure of the last Save/Reset, attributed to the draft it checked; only a still-current draft makes it actionable.
  issue:{fault:Fault; ticket:RecoveryTicket}|null;
  // The draft is the host's default-based Reset draft that could not be published as it stands.
  resetDraft:boolean;
  // A write committed but the follow-up catalog/context refresh failed; shown until a later refresh succeeds.
  refreshError:Fault|null;
}

export function sameRecoveryContext(a:RecoveryRef, b:RecoveryRef):boolean {
  return a.token === b.token && a.workspace.workspace_id === b.workspace.workspace_id && a.workspace.revision === b.workspace.revision;
}
// A retry reply keeps the token and workspace while the host moves the revision; every other context change is a new context.
export function retriedRecoveryContext(a:RecoveryRef, b:RecoveryRef):boolean {
  return a.token === b.token && a.workspace.workspace_id === b.workspace.workspace_id;
}

function chosen(view:RecoveryView, id:string|null) {
  return id === null ? undefined : view.profiles.find(entry => entry.profile.id === id);
}

function object(value:Json|undefined):value is Record<string,Json> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function segmentPath(segments:(string|number)[]):string {
  return segments.reduce<string>((path, segment) => typeof segment === 'number' ? `${path}[${segment}]` : optionPath(path, segment), '$');
}

function valueAt(draft:Json, segments:(string|number)[]):Json|undefined {
  let value:Json|undefined = draft;
  for (const segment of segments) {
    if (typeof segment === 'number') value = Array.isArray(value) ? value[segment] : undefined;
    else value = object(value) ? value[segment] : undefined;
    if (value === undefined) return undefined;
  }
  return value;
}

// Non-empty strings only: an empty stored string already fails numeric parsing, and Replace yields an empty string
// that must read as editor text so the field becomes editable.
export function storedText(schema:Schema, values:Json):StoredText[] {
  const found:StoredText[] = [];
  function walk(node:Schema, value:Json, segments:(string|number)[]) {
    if ((node.type === 'number' || node.type === 'integer') && typeof value === 'string' && value !== '') {
      found.push({segments, path:segmentPath(segments), text:value});
    } else if (node.type === 'object' && object(value)) {
      for (const [key, child] of Object.entries(value)) if (node.properties?.[key]) walk(node.properties[key], child, [...segments, key]);
    } else if (node.type === 'array' && Array.isArray(value) && node.items) {
      value.forEach((child, index) => walk(node.items!, child, [...segments, index]));
    }
  }
  walk(schema, values, []);
  return found;
}

// A field edit retires only its own stored mismatches. Array actions carry their indices explicitly: comparing values
// would confuse equal strings in different items, and cannot distinguish a move from an edit to an equal value.
function retainStoredText(previous:StoredText[], draft:Record<string,Json>, edit:RecoveryEdit):StoredText[] {
  const retained:StoredText[] = [];
  for (const entry of previous) {
    if (edit.kind === 'replace' && (entry.path === edit.path || entry.path.startsWith(`${edit.path}.`) || entry.path.startsWith(`${edit.path}[`))) continue;
    let segments = entry.segments;
    if (edit.kind !== 'replace') {
      const at = segments.findIndex((segment, index) => typeof segment === 'number' && segmentPath(segments.slice(0, index)) === edit.path);
      if (at >= 0) {
        const index = segments[at] as number;
        if (edit.kind === 'remove' && index === edit.index) continue;
        const next = edit.kind === 'remove' ? index > edit.index ? index - 1 : index
          : index === edit.index ? edit.other : index === edit.other ? edit.index : index;
        if (next !== index) segments = [...segments.slice(0, at), next, ...segments.slice(at + 1)];
      }
    }
    if (valueAt(draft, segments) === entry.text) retained.push({...entry, segments, path:segmentPath(segments)});
  }
  return retained;
}

function loaded(view:RecoveryView, values:Record<string,Json>|undefined) {
  return values ? {draft:structuredClone(values), storedText:storedText(view.package.schema, values)} : {draft:{}, storedText:[]};
}

function fresh(view:RecoveryView, draftRevision:number):RecoveryState {
  const first = view.profiles[0];
  return {view, selectedId:first?.profile.id ?? null, ...loaded(view, first?.profile.values),
    draftRevision:draftRevision + 1, touched:false, issue:null, resetDraft:false, refreshError:null};
}

// Numeric text parses as in the ordinary editor; stored type mismatches stay strings and block Save until replaced.
export function readRecoveryDraft(state:RecoveryState, locale:Locale) {
  return readDraft(state.view.package.schema, state.draft, locale, new Set(state.storedText.map(entry => entry.path)));
}

// A successful binding retry moves the context revision without changing its token or entries; the operator's
// unrepaired draft and its attribution follow it. Only an explicit retry reply may use this transition.
export function retriedRecovery(previous:RecoveryState, context:RecoveryRef):RecoveryState {
  if (!retriedRecoveryContext(previous.view.context, context)) return previous;
  const issue = previous.issue && retriedRecoveryContext(previous.issue.ticket.context, context)
    ? {...previous.issue, ticket:{...previous.issue.ticket, context}} : previous.issue;
  return {...previous, view:{...previous.view, context}, issue};
}

// A view for the same context keeps the operator's draft; a new or replaced context starts on its first rejected
// profile. A selected profile that is no longer rejected is left only while it carries unsaved edits.
export function recoveryState(view:RecoveryView, previous:RecoveryState|null):RecoveryState {
  if (!previous || !sameRecoveryContext(previous.view.context, view.context)) return fresh(view, previous?.draftRevision ?? 0);
  const next = {...previous, view};
  if (next.selectedId === null || chosen(view, next.selectedId) || next.touched) return next;
  return {...fresh(view, next.draftRevision), refreshError:next.refreshError};
}

// Selecting loads the stored safe values as a new draft; a stale ID (repaired or removed meanwhile) changes nothing.
export function selectRecovery(state:RecoveryState, id:string|null):RecoveryState {
  const entry = chosen(state.view, id);
  if (id !== null && !entry) return state;
  return {...state, selectedId:entry?.profile.id ?? null, ...loaded(state.view, entry?.profile.values),
    draftRevision:state.draftRevision + 1, touched:false, issue:null, resetDraft:false};
}

// Edits retire the attributed failure; the incomplete-Reset marker stays until the draft is saved or reloaded.
export function editRecovery(state:RecoveryState, draft:Record<string,Json>, edit:RecoveryEdit):RecoveryState {
  return {...state, draft, storedText:retainStoredText(state.storedText, draft, edit), draftRevision:state.draftRevision + 1, touched:true, issue:null};
}

// A draft is actionable only while its profile is still listed by the context; a detached newer draft has no ticket.
export function recoveryTicket(state:RecoveryState):RecoveryTicket|null {
  return state.selectedId === null || !chosen(state.view, state.selectedId) ? null
    : {context:state.view.context, profileId:state.selectedId, draftRevision:state.draftRevision};
}

export function currentRecoveryDraft(state:RecoveryState, ticket:RecoveryTicket):boolean {
  return sameRecoveryContext(state.view.context, ticket.context) && state.selectedId === ticket.profileId && state.draftRevision === ticket.draftRevision;
}

// Only a current reply may replace the draft; context retirement requires explicit Close/Reinspect. The refresh
// warning changes only with a reply that actually refreshed (a catalog) or failed to (a refresh fault).
export function mutatedRecovery(state:RecoveryState, ticket:RecoveryTicket, mutation:RecoveryMutation):RecoveryState {
  if (!sameRecoveryContext(state.view.context, ticket.context)) return state;
  const current = currentRecoveryDraft(state, ticket);
  let next:RecoveryState = mutation.catalog !== null || mutation.refresh_error !== null ? {...state, refreshError:mutation.refresh_error} : state;
  const saved = mutation.saved;
  if (saved !== null) {
    if (current) next = {...next, touched:false, issue:null, resetDraft:false};
    const view = mutation.recovery ?? {...state.view, profiles:state.view.profiles.filter(entry => entry.profile.id !== saved.id)};
    return recoveryState(view, next);
  }
  if (mutation.issue !== null) {
    if (current && mutation.draft !== null) {
      // The host's default-based draft replaces the checked one; its failure belongs to that new draft.
      const draftRevision = next.draftRevision + 1;
      next = {...next, draft:mutation.draft, storedText:storedText(state.view.package.schema, mutation.draft), draftRevision,
        touched:false, resetDraft:true, issue:{fault:mutation.issue, ticket:{...ticket, draftRevision}}};
    } else {
      next = {...next, issue:{fault:mutation.issue, ticket}};
    }
  }
  return mutation.recovery ? recoveryState(mutation.recovery, next) : next;
}

// Per-profile facts are keyed by profile; a later fact for the same profile replaces the earlier one in place.
export function upsertOutcomes(current:ProfileRecoveryOutcome[], incoming:ProfileRecoveryOutcome[]):ProfileRecoveryOutcome[] {
  if (incoming.length === 0) return current;
  const next = [...current];
  for (const outcome of incoming) {
    const index = next.findIndex(item => item.profile_id === outcome.profile_id);
    if (index < 0) next.push(outcome); else next[index] = outcome;
  }
  return next;
}

// The failure to show for the selected profile: an attributed Save/Reset result wins over the listing's issue.
export function recoveryIssue(state:RecoveryState):{fault:Fault; earlier:boolean}|null {
  if (state.issue) return {fault:state.issue.fault, earlier:!currentRecoveryDraft(state, state.issue.ticket)};
  const entry = chosen(state.view, state.selectedId);
  return entry ? {fault:entry.issue, earlier:false} : null;
}

// Structured value path the host attached to a schema/value fault; never parsed from the message text.
export function issuePath(fault:Fault):string|null {
  const context = fault.context;
  if (context === null || typeof context !== 'object' || Array.isArray(context)) return null;
  const path = context.path;
  return typeof path === 'string' && (path === '$' || path.startsWith('$.') || path.startsWith('$[')) ? path : null;
}

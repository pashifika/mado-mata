import type {Message} from './i18n.ts';
import type {AuthoringFile, AuthoringFileKind, AuthoringMutation, AuthoringRef, AuthoringValidation, AuthoringView, CatalogEdit, Fault} from './types.ts';
import type {RecognitionState} from './recognition.ts';

// Host fault categories the editor reacts to; every other category is shown as an ordinary action failure.
export const AUTHORING_ACTIVE = 'AuthoringActive';
export const AUTHORING_CONFLICT = 'AuthoringConflict';
export const AUTHORING_RECOVERY = 'AuthoringRecoveryRequired';
// Per-file history is bounded by entries and retained characters so one large file cannot grow memory without limit.
export const HISTORY_ENTRIES = 200;
export const HISTORY_CHARACTERS = 4_000_000;
// Search counting stops here; navigation still reaches every match.
export const MATCH_LIMIT = 10_000;
// Rust authoring's text-file limit and the inventory's package non-image allowance.
export const AUTHORING_SOURCE_BYTES = 1_048_576;
export const AUTHORING_NON_IMAGE_BYTES = 1_048_576;

export interface TextRange {start:number; end:number}
// Structured metadata snapshots also carry the draft's form provenance (see `typed`), so Undo and Redo restore
// incomplete numeric input together with its text. Source snapshots have none.
export interface Snapshot extends TextRange {text:string; typed?:TypedText[]}
// Text the operator typed into a numeric field of a structured form that is not a number yet (e.g. `1.`), at a value
// path of that form. The document holds it as a string; this marks it as editable text rather than stored data.
export interface TypedText {path:string; text:string}

// One declared file's editor state. `base` is the last known disk text (the host's latest read or this session's
// committed Save); dirtiness is `text !== base`, so undoing back to the saved text is clean again. Binary assets have
// null text and are listed only. `revision` counts local edits: a reply for an older draft never marks a newer one saved.
export interface FileDraft {
  path:string; kind:AuthoringFileKind; bytes:number;
  base:string|null; text:string|null; revision:number;
  undo:Snapshot[]; redo:Snapshot[];
  // Coalescing key of the last edit, so ordinary typing forms word-sized undo steps.
  group:string|null;
  // Text and selection before an active IME composition; its undo step is recorded only when composition ends.
  composing:Snapshot|null;
  range:TextRange;
  // A deliberate refresh found other disk text under this unsaved draft; Save replaces it, Discard adopts it.
  diskChanged:boolean;
  // The file is no longer declared; the unsaved text is kept for copying but cannot be saved.
  missing:boolean;
  // Form provenance for this draft's text, so a form shown again (after another file, Logs or settings) resumes the
  // operator's numeric text. Discard, a changed disk read and a new session (Open, Create, Duplicate) clear it.
  typed:TypedText[];
}

export type AuthoringPending = {kind:'save'; path:string} | {kind:'catalog'} | {kind:'refresh'} | {kind:'validate'} | {kind:'recognition'} | {kind:'recognition_trial'} | {kind:'exit'} | {kind:'duplicate'};
export interface ValidationState {revision:string; valid:boolean; diagnostics:Fault[]}

// The frontend half of one host-issued Edit lease. `revision` is the source revision every draft is based on and
// the one the next Save/catalog edit expects. Replies are applied only for the same owner token.
export interface AuthoringSession {
  owner:AuthoringRef; packagePath:string; packageId:string; revision:string;
  order:string[]; drafts:Map<string,FileDraft>; selected:string|null;
  destination:'file'|'recognition';
  recognition:RecognitionState|null;
  // Bumped whenever the stored selection must be applied to the editor (file switch, undo, search, diagnostics).
  reveal:number;
  pending:AuthoringPending|null;
  // Publication committed but its fresh view is unknown: further writes wait for an explicit refresh.
  refreshRequired:boolean;
  // A publication was refused because the source changed; drafts wait for a deliberate refresh.
  conflict:boolean;
  refreshError:Fault|null; error:Fault|null; notice:Message|null;
  validation:ValidationState|null;
}

export interface SaveTicket {token:string; path:string; expected:string; draftRevision:number; text:string}
export interface CatalogTicket {token:string; expected:string; edit:CatalogEdit}
export interface ValidationTicket {token:string; revision:string}
export interface EditInput {type:string; data:string|null; composing:boolean}
export interface ReplacementTicket {owner:string; path:string; revision:number}
export type ReplacementResult = {kind:'edit'; next:Snapshot} | {kind:'select'; range:TextRange} | {kind:'noop'}
  | {kind:'refused'; reason:'stale'|'ineligible'|'oversized'};
export type PublishBlock = 'pending' | 'refresh' | 'missing' | 'clean' | 'binary' | 'manifestDirty' | 'fileDirty';

export function sameAuthoringRef(a:AuthoringRef|null|undefined, b:AuthoringRef|null|undefined):boolean {
  if (!a || !b) return a === b;
  return a.token === b.token && a.workspace.workspace_id === b.workspace.workspace_id && a.workspace.revision === b.workspace.revision;
}

export function shortRevision(revision:string):string {
  return revision.length > 12 ? revision.slice(0, 12) : revision;
}

function clamp(range:TextRange, length:number):TextRange {
  const start = Math.min(Math.max(0, range.start), length);
  return {start, end: Math.min(Math.max(start, range.end), length)};
}

function freshDraft(file:AuthoringFile, range:TextRange = {start: 0, end: 0}):FileDraft {
  return {path: file.path, kind: file.kind, bytes: file.bytes, base: file.text, text: file.text, revision: 0, undo: [], redo: [],
    group: null, composing: null, range: clamp(range, file.text?.length ?? 0), diskChanged: false, missing: false, typed: []};
}

export function fileDirty(draft:FileDraft):boolean {
  return draft.text !== null && draft.text !== draft.base;
}

// Declared files in host order, then kept drafts of files that are no longer declared.
export function draftList(session:AuthoringSession):FileDraft[] {
  const listed = session.order.flatMap(path => session.drafts.get(path) ?? []);
  return [...listed, ...[...session.drafts.values()].filter(draft => draft.missing)];
}

export function dirtyDrafts(session:AuthoringSession):FileDraft[] {
  return draftList(session).filter(fileDirty);
}

function firstEditable(order:string[], drafts:Map<string,FileDraft>):string|null {
  return order.find(path => (drafts.get(path)?.text ?? null) !== null) ?? order[0] ?? null;
}

export function openSession(view:AuthoringView, notice:Message|null = null):AuthoringSession {
  const drafts = new Map(view.files.map(file => [file.path, freshDraft(file)] as const));
  const order = view.files.map(file => file.path);
  const manifest = view.files.find(file => file.kind === 'manifest')?.path;
  const selected = order.find(path => drafts.get(path)?.kind === 'source') ?? manifest ?? firstEditable(order, drafts);
  return {owner: view.owner, packagePath: view.package_path, packageId: view.package_id, revision: view.revision, order, drafts, selected,
    destination: 'file', recognition: null, reveal: 0, pending: null, refreshRequired: false, conflict: false, refreshError: null, error: null, notice, validation: null};
}

function withDraft(session:AuthoringSession, draft:FileDraft):AuthoringSession {
  const drafts = new Map(session.drafts);
  drafts.set(draft.path, draft);
  return {...session, drafts};
}

function pushHistory(stack:Snapshot[], entry:Snapshot):Snapshot[] {
  const next = [...stack, entry];
  let characters = next.reduce((total, item) => total + item.text.length, 0);
  while (next.length > 1 && (next.length > HISTORY_ENTRIES || characters > HISTORY_CHARACTERS)) {
    characters -= next.shift()!.text.length;
  }
  return next;
}

// `text` is the draft's current text at `range`, with form provenance for structured metadata.
function snapshot(draft:FileDraft, text:string, range:TextRange = draft.range):Snapshot {
  const at = clamp(range, text.length);
  return draft.kind === 'source' ? {text, ...at} : {text, ...at, typed: draft.typed};
}

// Ordinary typing and repeated deletion coalesce while the caret stays where the last edit left it; whitespace,
// paste, line breaks and anything unrecognized are separate steps. Composition text is one step per composition,
// including a committing input that WebKit may deliver after compositionend.
function editGroup(input:EditInput):string|null {
  if (input.composing || input.type === 'insertCompositionText' || input.type === 'insertFromComposition') return 'composition';
  if (input.type === 'insertText' && input.data !== null && input.data !== '' && !/\s/u.test(input.data)) return 'insert';
  return input.type === 'deleteContentBackward' || input.type === 'deleteContentForward' ? input.type : null;
}

export function editFile(session:AuthoringSession, path:string, next:Snapshot, before:TextRange, input:EditInput):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null) return session;
  const range = clamp(next, next.text.length);
  if (next.text === draft.text) return withDraft(session, {...draft, range});
  if (draft.composing !== null) return withDraft(session, {...draft, text: next.text, range, revision: draft.revision + 1});
  const group = editGroup(input);
  const contiguous = before.start === draft.range.start && before.end === draft.range.end;
  const coalesce = group !== null && group === draft.group && contiguous;
  const undo = coalesce ? draft.undo : pushHistory(draft.undo, snapshot(draft, draft.text, before));
  return withDraft(session, {...draft, text: next.text, range, revision: draft.revision + 1, undo, redo: [], group});
}

export function beginComposition(session:AuthoringSession, path:string, range:TextRange):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null || draft.composing !== null) return session;
  return withDraft(session, {...draft, composing: snapshot(draft, draft.text, range)});
}

// A trailing composition input after compositionend (WebKit ordering) merges into this step via the group key.
export function endComposition(session:AuthoringSession, path:string):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null || draft.composing === null) return session;
  const composing = draft.composing;
  const sameTyped = composing.typed === undefined || (composing.typed.length === draft.typed.length
    && composing.typed.every((entry, index) => entry.path === draft.typed[index].path && entry.text === draft.typed[index].text));
  if (composing.text === draft.text && sameTyped) return withDraft(session, {...draft, composing: null});
  return withDraft(session, {...draft, composing: null, undo: pushHistory(draft.undo, composing), redo: [], group: 'composition'});
}

function travel(session:AuthoringSession, path:string, direction:'undo'|'redo'):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null || draft.composing !== null) return session;
  const from = direction === 'undo' ? draft.undo : draft.redo;
  const target = from.at(-1);
  if (!target) return session;
  const remaining = from.slice(0, -1);
  const other = pushHistory(direction === 'undo' ? draft.redo : draft.undo, snapshot(draft, draft.text));
  const moved:FileDraft = {...draft, text: target.text, range: clamp(target, target.text.length), revision: draft.revision + 1, group: null,
    typed: target.typed ?? draft.typed, undo: direction === 'undo' ? remaining : other, redo: direction === 'undo' ? other : remaining};
  return {...withDraft(session, moved), reveal: session.reveal + 1};
}

export function undoFile(session:AuthoringSession, path:string):AuthoringSession {
  return travel(session, path, 'undo');
}

export function redoFile(session:AuthoringSession, path:string):AuthoringSession {
  return travel(session, path, 'redo');
}

// One accepted external (agent) transaction. Each changed text draft gets exactly one undo step that never coalesces
// with adjacent typing; the caller supplies the caret mapped through its change. Selection, destination and reveal stay
// unchanged, so the editor synchronizes the text without moving focus, the caret or the selected file.
export interface ExternalEdit {path:string; text:string; range:TextRange}

export function applyExternalEdits(session:AuthoringSession, edits:readonly ExternalEdit[]):AuthoringSession {
  let drafts:Map<string,FileDraft>|null = null;
  for (const edit of edits) {
    const draft = (drafts ?? session.drafts).get(edit.path);
    if (!draft || draft.text === null || draft.composing !== null || draft.text === edit.text) continue;
    drafts ??= new Map(session.drafts);
    drafts.set(edit.path, {...draft, text: edit.text, range: clamp(edit.range, edit.text.length), revision: draft.revision + 1,
      undo: pushHistory(draft.undo, snapshot(draft, draft.text)), redo: [], group: null});
  }
  return drafts === null ? session : {...session, drafts};
}

// Structured metadata forms replace the whole document. Each replacement is one undo step with the draft's form
// provenance; during a form's IME composition the text follows the input and the composition-start snapshot becomes
// the single step when it ends. Dirtiness stays `text !== base`, so restoring every value is clean. A form that tracks
// numeric text passes its provenance; other structured edits keep the recorded one, which a form applies only where
// the document still holds exactly that text. Provenance-only changes are visible form edits and have their own step.
export function replaceFile(session:AuthoringSession, path:string, text:string, typed?:readonly TypedText[]):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null) return session;
  const provenance = typed === undefined ? draft.typed : [...typed];
  const sameProvenance = provenance.length === draft.typed.length
    && provenance.every((entry, index) => entry.path === draft.typed[index].path && entry.text === draft.typed[index].text);
  if (draft.text === text && sameProvenance) return session;
  if (draft.composing !== null) return withDraft(session, {...draft, text, revision: draft.revision + 1, typed: provenance});
  return withDraft(session, {...draft, text, range: {start: 0, end: 0}, revision: draft.revision + 1, group: null, typed: provenance,
    undo: pushHistory(draft.undo, snapshot(draft, draft.text)), redo: []});
}

// Discarding one file is itself undoable; a draft of an undeclared file is dropped.
export function discardFile(session:AuthoringSession, path:string):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || !fileDirty(draft) || draft.composing !== null) return session;
  if (draft.missing) {
    const drafts = new Map(session.drafts);
    drafts.delete(path);
    const selected = session.selected === path ? firstEditable(session.order, drafts) : session.selected;
    return {...session, drafts, selected, reveal: session.reveal + 1, notice: {key: 'authoringDiscarded', args: [path]}};
  }
  const base = draft.base ?? '';
  const discarded:FileDraft = {...draft, text: base, range: clamp(draft.range, base.length), revision: draft.revision + 1, group: null,
    undo: pushHistory(draft.undo, snapshot(draft, draft.text!)), redo: [], diskChanged: false, typed: []};
  return {...withDraft(session, discarded), reveal: session.reveal + 1, notice: {key: 'authoringDiscarded', args: [path]}};
}

// Keeps the outgoing file's selection so returning to it restores the caret.
export function selectFile(session:AuthoringSession, path:string, previous:TextRange|null):AuthoringSession {
  if (!session.drafts.has(path) || (session.selected === path && session.destination === 'file')) return session;
  const outgoing = session.selected === null ? undefined : session.drafts.get(session.selected);
  const kept = outgoing && previous ? recordRange(session, outgoing.path, previous) : session;
  return {...kept, selected: path, destination: 'file', reveal: session.reveal + 1};
}

// A caret recorded somewhere else than the last edit left it also ends the current typing undo step.
export function recordRange(session:AuthoringSession, path:string, range:TextRange):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null) return session;
  const next = clamp(range, draft.text.length);
  return next.start === draft.range.start && next.end === draft.range.end ? session : withDraft(session, {...draft, range: next, group: null});
}

// Selects a range in a file without changing its text (search matches and diagnostic locations).
export function revealRange(session:AuthoringSession, path:string, range:TextRange):AuthoringSession {
  const draft = session.drafts.get(path);
  if (!draft || draft.text === null) return session;
  return {...withDraft(session, {...draft, range: clamp(range, draft.text.length), group: null}), selected: path, destination: 'file', reveal: session.reveal + 1};
}

export function selectRecognition(session:AuthoringSession, previous:TextRange|null):AuthoringSession {
  const kept = session.selected && previous ? recordRange(session, session.selected, previous) : session;
  return {...kept, destination: 'recognition'};
}

export function beginPending(session:AuthoringSession, pending:AuthoringPending):AuthoringSession {
  return {...session, pending, error: null, notice: session.notice?.key === 'authoringStopRequested' ? null : session.notice};
}

export function saveBlock(session:AuthoringSession, path:string):PublishBlock|null {
  const draft = session.drafts.get(path);
  if (session.pending !== null) return 'pending';
  if (!draft || draft.text === null) return 'binary';
  if (session.refreshRequired || session.conflict) return 'refresh';
  if (draft.missing) return 'missing';
  return fileDirty(draft) ? null : 'clean';
}

export function saveTicket(session:AuthoringSession, path:string):SaveTicket|null {
  const draft = session.drafts.get(path);
  if (saveBlock(session, path) !== null || !draft || draft.text === null) return null;
  return {token: session.owner.token, path, expected: session.revision, draftRevision: draft.revision, text: draft.text};
}

// Merges a fresh host read without erasing unsaved text: clean drafts adopt the disk text, dirty drafts keep theirs
// and record whether the disk moved underneath, and dirty drafts of files that disappeared are kept as missing.
function mergeView(session:AuthoringSession, view:AuthoringView):{session:AuthoringSession; changed:number} {
  const drafts = new Map<string,FileDraft>();
  let changed = 0;
  for (const file of view.files) {
    const old = session.drafts.get(file.path);
    if (file.text === null || !old || old.text === null) {
      drafts.set(file.path, freshDraft(file, old?.range));
    } else if (!fileDirty(old)) {
      drafts.set(file.path, old.text === file.text ? {...old, kind: file.kind, bytes: file.bytes, base: file.text, diskChanged: false, missing: false} : freshDraft(file, old.range));
    } else {
      if (old.base !== file.text) changed += 1;
      drafts.set(file.path, {...old, kind: file.kind, bytes: file.bytes, base: file.text, diskChanged: old.diskChanged || old.base !== file.text, missing: false});
    }
  }
  for (const [path, old] of session.drafts) {
    if (!drafts.has(path) && fileDirty(old)) drafts.set(path, {...old, missing: true});
  }
  const order = view.files.map(file => file.path);
  const selected = session.selected !== null && drafts.has(session.selected) ? session.selected : firstEditable(order, drafts);
  return {session: {...session, packagePath: view.package_path, packageId: view.package_id, revision: view.revision, order, drafts, selected}, changed};
}

// The commit is truth even when the follow-up read failed: the saved text becomes the file's base, and a later
// draft stays unsaved and dirty. Only the same owner token may apply the reply.
export function applySave(session:AuthoringSession|null, ticket:SaveTicket, mutation:AuthoringMutation):AuthoringSession|null {
  if (!session || session.owner.token !== ticket.token || mutation.owner.token !== ticket.token) return session;
  const draft = session.drafts.get(ticket.path);
  const current = draft !== undefined && draft.revision === ticket.draftRevision;
  let next:AuthoringSession = {...session, revision: mutation.committed_revision, pending: null, error: null, conflict: false};
  if (draft) next = withDraft(next, {...draft, base: ticket.text, diskChanged: false, missing: false});
  const revision = shortRevision(mutation.committed_revision);
  if (mutation.view === null || mutation.view.owner.token !== ticket.token) {
    return {...next, refreshRequired: true, refreshError: mutation.refresh_error, notice: {key: 'authoringSavedRefreshFailed', args: [ticket.path, revision]}};
  }
  return {...mergeView(next, mutation.view).session, refreshRequired: false, refreshError: null,
    notice: current ? {key: 'authoringSaved', args: [ticket.path, revision]} : {key: 'authoringEarlierSaved', args: [ticket.path, revision]}};
}

export function failCommand(session:AuthoringSession|null, token:string, error:Fault):AuthoringSession|null {
  if (!session || session.owner.token !== token) return session;
  const conflict = error.category === AUTHORING_CONFLICT;
  return {...session, pending: null, error, conflict: session.conflict || conflict, notice: conflict ? {key: 'authoringConflict'} : null};
}

// Catalog edits rewrite the manifest, so a dirty manifest draft or a dirty file being renamed/removed must be
// saved or discarded first; the host still performs its own owned-path and reference checks.
export function catalogBlock(session:AuthoringSession, edit:CatalogEdit):PublishBlock|null {
  if (session.pending !== null) return 'pending';
  if (session.refreshRequired || session.conflict) return 'refresh';
  if (draftList(session).some(draft => draft.kind === 'manifest' && fileDirty(draft))) return 'manifestDirty';
  if (edit.kind !== 'add') {
    const target = session.drafts.get(edit.path);
    if (target && fileDirty(target)) return 'fileDirty';
  }
  return null;
}

export function catalogTicket(session:AuthoringSession, edit:CatalogEdit):CatalogTicket|null {
  return catalogBlock(session, edit) === null ? {token: session.owner.token, expected: session.revision, edit} : null;
}

export function applyCatalogMutation(session:AuthoringSession|null, ticket:CatalogTicket, mutation:AuthoringMutation, foreground:boolean):AuthoringSession|null {
  if (!session || session.owner.token !== ticket.token || mutation.owner.token !== ticket.token) return session;
  const edit = ticket.edit;
  let next:AuthoringSession = {...session, revision: mutation.committed_revision, pending: null, error: null, conflict: false};
  // A renamed file keeps its history and caret under the destination path.
  const renamed = edit.kind === 'rename' ? session.drafts.get(edit.path) : undefined;
  if (edit.kind === 'rename' && renamed) {
    const drafts = new Map(next.drafts);
    drafts.delete(edit.path);
    drafts.set(edit.destination, {...renamed, path: edit.destination});
    next = {...next, drafts, order: next.order.map(path => path === edit.path ? edit.destination : path),
      selected: session.selected === edit.path ? edit.destination : session.selected};
  }
  const revision = shortRevision(mutation.committed_revision);
  if (mutation.view === null || mutation.view.owner.token !== ticket.token) {
    return {...next, refreshRequired: true, refreshError: mutation.refresh_error, notice: {key: 'authoringCatalogRefreshFailed', args: [revision]}};
  }
  const merged = mergeView(next, mutation.view).session;
  const added = foreground && edit.kind === 'add' && merged.drafts.has(edit.path);
  const selected = added ? edit.path : merged.selected;
  return {...merged, selected, destination: added ? 'file' : merged.destination, reveal: session.reveal + (foreground ? 1 : 0),
    refreshRequired: false, refreshError: null, notice: {key: 'authoringCatalogSaved', args: [revision]}};
}

// Recognition publishes declared metadata/assets through the same source revision without replacing text drafts.
export function applyRecognitionMutation(session:AuthoringSession|null, mutation:AuthoringMutation):AuthoringSession|null {
  if (!session || !sameAuthoringRef(session.owner, mutation.owner)) return session;
  const next:AuthoringSession = {...session, revision: mutation.committed_revision, pending: null, error: null, conflict: false};
  if (mutation.view === null || !sameAuthoringRef(mutation.view.owner, session.owner)) {
    return {...next, refreshRequired: true, refreshError: mutation.refresh_error,
      notice: {key: 'authoringCatalogRefreshFailed', args: [shortRevision(mutation.committed_revision)]}};
  }
  return {...mergeView(next, mutation.view).session, refreshRequired: false, refreshError: null, notice: {key: 'recognitionSaved'}};
}

export function applyRefresh(session:AuthoringSession|null, view:AuthoringView, foreground:boolean):AuthoringSession|null {
  if (!session || session.owner.token !== view.owner.token) return session;
  const {session: merged, changed} = mergeView(session, view);
  return {...merged, reveal: session.reveal + (foreground ? 1 : 0), pending: null, refreshRequired: false, conflict: false, refreshError: null, error: null,
    notice: changed > 0 ? {key: 'authoringDiskChanged', args: [changed]} : {key: 'authoringRefreshed', args: [shortRevision(view.revision)]}};
}

export function validationTicket(session:AuthoringSession):ValidationTicket|null {
  return session.pending === null ? {token: session.owner.token, revision: session.revision} : null;
}

// The trusted compiler returns an envelope; expose its individual source diagnostics rather than only "compilation failed".
function expandDiagnostic(fault:Fault):Fault[] {
  const context = fault.context;
  if (fault.category !== 'TypeScript' || context === null || typeof context !== 'object' || Array.isArray(context) || !Array.isArray(context.diagnostics)) return [fault];
  const diagnostics:Fault[] = [];
  for (const item of context.diagnostics) {
    if (item === null || typeof item !== 'object' || Array.isArray(item) || typeof item.message !== 'string') continue;
    diagnostics.push({category: fault.category, message: item.message,
      context: {path: item.module ?? null, line: item.line ?? null, column: item.column ?? null, code: item.code ?? null}});
  }
  return diagnostics.length > 0 ? diagnostics : [fault];
}

// A result names the revision it captured; it never describes a later save or the unsaved drafts. Validation reserves
// its work before capturing the disk, so a source change surfaces as a conflict diagnostic rather than a refusal.
export function applyValidation(session:AuthoringSession|null, ticket:ValidationTicket, result:AuthoringValidation):AuthoringSession|null {
  if (!session || session.owner.token !== ticket.token || result.owner.token !== ticket.token) return session;
  const validation = {revision: result.revision, valid: result.valid, diagnostics: result.diagnostics.flatMap(expandDiagnostic)};
  const revision = shortRevision(result.revision);
  const conflict = result.diagnostics.some(item => item.category === AUTHORING_CONFLICT);
  const notice:Message = conflict ? {key: 'authoringConflict'}
    : result.revision !== session.revision ? {key: 'authoringEarlierValidated', args: [revision]}
    : result.valid ? {key: 'authoringValidated', args: [revision]} : {key: 'authoringInvalid', args: [revision, validation.diagnostics.length]};
  return {...session, pending: null, error: null, conflict: session.conflict || conflict, validation, notice};
}

// The package directory an interrupted-save refusal names, falling back to the directory the operator requested.
export function recoveryPath(fault:Fault, fallback:string):string {
  const context = fault.context;
  const path = context !== null && typeof context === 'object' && !Array.isArray(context) ? context.package_path : undefined;
  return typeof path === 'string' && path ? path : fallback;
}

export function validationCurrent(session:AuthoringSession):boolean {
  return session.validation !== null && session.validation.revision === session.revision;
}

export interface SourceLocation {path:string; line:number; column:number}

// Diagnostic context may name a file and a 1-based line/column; anything else stays an unlocated diagnostic.
export function diagnosticLocation(fault:Fault):SourceLocation|null {
  const context = fault.context;
  if (context === null || typeof context !== 'object' || Array.isArray(context)) return null;
  const {path, line, column} = context;
  if (typeof path !== 'string' || !path) return null;
  const lineNumber = typeof line === 'number' && Number.isSafeInteger(line) && line > 0 ? line : 1;
  const columnNumber = typeof column === 'number' && Number.isSafeInteger(column) && column > 0 ? column : 1;
  return {path, line: lineNumber, column: columnNumber};
}

// Retired drawer callbacks cannot navigate a replacement lease or a newer diagnostic result.
export function revealDiagnostic(session:AuthoringSession, token:string, fault:Fault):AuthoringSession {
  if (session.owner.token !== token || !session.validation?.diagnostics.includes(fault)) return session;
  const location = diagnosticLocation(fault);
  const target = location ? session.drafts.get(location.path) : undefined;
  if (!location || !target || target.missing) return session;
  if (target.kind !== 'source' || target.text === null) {
    const selected = selectFile(session, location.path, null);
    return selected === session ? {...session, reveal: session.reveal + 1} : selected;
  }
  const offset = offsetAt(target.text, location.line, location.column);
  return revealRange(session, location.path, {start: offset, end: offset});
}

export function offsetAt(text:string, line:number, column:number):number {
  const breaks = /\r\n|\r|\n/g;
  let offset = 0;
  for (let current = 1; current < line; current++) {
    const next = breaks.exec(text);
    if (!next) return text.length;
    offset = next.index + next[0].length;
  }
  const end = breaks.exec(text)?.index ?? text.length;
  return Math.min(offset + column - 1, end);
}

export function lineColumn(text:string, offset:number):{line:number; column:number} {
  const breaks = /\r\n|\r|\n/g;
  let line = 1;
  let start = 0;
  for (let next = breaks.exec(text); next && next.index + next[0].length <= offset; next = breaks.exec(text)) {
    line += 1;
    start = next.index + next[0].length;
  }
  return {line, column: offset - start + 1};
}

export function lineCount(text:string):number {
  const breaks = /\r\n|\r|\n/g;
  let count = 1;
  while (breaks.exec(text)) count += 1;
  return count;
}

// Literal, case-insensitive search over UTF-16 offsets.
function pattern(query:string):RegExp {
  return new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi');
}

// Next match after `range`, or the previous one before it; both wrap around the file.
export function findMatch(text:string, query:string, range:TextRange, backward:boolean):TextRange|null {
  if (!query) return null;
  const search = pattern(query);
  if (!backward) {
    search.lastIndex = range.end;
    let match = search.exec(text);
    if (!match) {
      search.lastIndex = 0;
      match = search.exec(text);
    }
    return match ? {start: match.index, end: match.index + match[0].length} : null;
  }
  let previous:TextRange|null = null;
  let last:TextRange|null = null;
  for (let match = search.exec(text); match; match = search.exec(text)) {
    last = {start: match.index, end: match.index + match[0].length};
    if (match.index < range.start) previous = last;
  }
  return previous ?? last;
}

// Count of matches (bounded) and the 1-based position of the selected one, if the selection is a match.
export function matchSummary(text:string, query:string, range:TextRange):{count:number; capped:boolean; current:number|null} {
  if (!query) return {count: 0, capped: false, current: null};
  const search = pattern(query);
  let count = 0;
  let current:number|null = null;
  for (let match = search.exec(text); match; match = search.exec(text)) {
    count += 1;
    if (match.index === range.start && match.index + match[0].length === range.end) current = count;
    if (count >= MATCH_LIMIT) return {count, capped: true, current};
  }
  return {count, capped: false, current};
}

function highSurrogate(unit:number):boolean {
  return unit >= 0xd800 && unit <= 0xdbff;
}

function lowSurrogate(unit:number):boolean {
  return unit >= 0xdc00 && unit <= 0xdfff;
}

// Match TextEncoder without allocating an encoded copy, including unpaired UTF-16 surrogates.
export function utf8Bytes(text:string, start = 0, end = text.length):number {
  let bytes = 0;
  for (let index = start; index < end; index++) {
    const unit = text.charCodeAt(index);
    if (unit < 0x80) bytes += 1;
    else if (unit < 0x800) bytes += 2;
    else if (highSurrogate(unit) && index + 1 < end && lowSurrogate(text.charCodeAt(index + 1))) {
      bytes += 4;
      index += 1;
    } else bytes += 3;
  }
  return bytes;
}

// Present disk-file drafts only; host Save/Validate additionally count the trusted dependency closure.
export function otherNonImageBytes(session:AuthoringSession, path:string):number {
  const images = new Set<string>();
  const manifest = [...session.drafts.values()].find(draft => draft.kind === 'manifest' && !draft.missing);
  // Unsaved manifest changes must not reclassify assets owned by the current host inventory.
  if (manifest?.base !== null && manifest?.base !== undefined) {
    try {
      const value = JSON.parse(manifest.base);
      if (value !== null && typeof value === 'object' && value.assets !== null && typeof value.assets === 'object') {
        for (const asset of Object.values(value.assets)) {
          if (asset !== null && typeof asset === 'object' && 'path' in asset && typeof asset.path === 'string'
            && 'format' in asset && (asset.format === 'png' || asset.format === 'raw-rgba8')) images.add(asset.path);
        }
      }
    } catch {
      // A view without a valid saved manifest cannot grant the image-byte exemption.
    }
  }
  let bytes = 0;
  for (const draft of session.drafts.values()) {
    if (draft.path === path || draft.missing || (draft.kind === 'asset' && images.has(draft.path))) continue;
    bytes += draft.text === null ? draft.bytes : utf8Bytes(draft.text);
  }
  return bytes;
}

// Produces a draft transaction only; the caller retains edit/history and publication ownership.
export function replacementEdit(session:AuthoringSession|null, ticket:ReplacementTicket, query:string, replacement:string,
  all:boolean, range:TextRange):ReplacementResult {
  if (!session || session.owner.token !== ticket.owner || session.selected !== ticket.path) return {kind: 'refused', reason: 'stale'};
  const draft = session.drafts.get(ticket.path);
  if (draft && draft.revision !== ticket.revision) return {kind: 'refused', reason: 'stale'};
  if (!draft || draft.kind !== 'source' || draft.text === null || draft.missing || draft.composing !== null
    || session.destination !== 'file' || session.pending?.kind === 'exit' || session.pending?.kind === 'duplicate') {
    return {kind: 'refused', reason: 'ineligible'};
  }
  if (!query) return {kind: 'noop'};
  const original = draft.text;
  const search = pattern(query);
  if (!all) search.lastIndex = range.start;
  const first = search.exec(original);
  if (!all && (!first || first.index !== range.start || first.index + first[0].length !== range.end)) {
    const next = findMatch(original, query, range, false);
    return next ? {kind: 'select', range: next} : {kind: 'noop'};
  }
  if (!first) return {kind: 'noop'};

  const replacementBytes = utf8Bytes(replacement);
  let bytes = 0;
  let lastUnit = NaN;
  let changed = false;
  let end = 0;
  // Segment boundaries can split or join surrogate pairs; count the actual joined output.
  function append(text:string, start:number, stop:number, size = utf8Bytes(text, start, stop)) {
    if (start === stop) return;
    bytes += size - (highSurrogate(lastUnit) && lowSurrogate(text.charCodeAt(start)) ? 2 : 0);
    lastUnit = text.charCodeAt(stop - 1);
  }
  for (let match:RegExpExecArray|null = first; match; match = all ? search.exec(original) : null) {
    append(original, end, match.index);
    append(replacement, 0, replacement.length, replacementBytes);
    end = match.index + match[0].length;
    changed ||= match[0] !== replacement;
  }
  append(original, end, original.length);
  if (!changed) return {kind: 'noop'};
  if (bytes > AUTHORING_SOURCE_BYTES || bytes + otherNonImageBytes(session, ticket.path) > AUTHORING_NON_IMAGE_BYTES) {
    return {kind: 'refused', reason: 'oversized'};
  }
  // Allocation follows the complete preflight; callback replacement keeps dollar signs and backslashes literal.
  const text = all ? original.replace(pattern(query), () => replacement)
    : original.slice(0, first.index) + replacement + original.slice(first.index + first[0].length);
  if (text === original) return {kind: 'noop'};
  const caret = first.index + replacement.length;
  return {kind: 'edit', next: {text, start: caret, end: caret}};
}

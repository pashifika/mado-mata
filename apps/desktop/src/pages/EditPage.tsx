import {Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {flushSync} from 'react-dom';
import CatalogDialog from '../components/CatalogDialog.tsx';
import type {CatalogIntent} from '../components/CatalogDialog.tsx';
import ContextMenu, {elementAnchor, menuEvents} from '../components/ContextMenu.tsx';
import type {MenuAction, MenuAnchor} from '../components/ContextMenu.tsx';
import {ButtonHint, HelpTrigger} from '../components/ContextualHelp.tsx';
import FileTree from '../components/FileTree.tsx';
import ManifestEditor from '../components/ManifestEditor.tsx';
import Modal from '../components/Modal.tsx';
import {AssetView, SourceMapView} from '../components/MetadataFacts.tsx';
import PresetEditor from '../components/PresetEditor.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import SchemaEditor from '../components/SchemaEditor.tsx';
import SourceEditor from '../editor/SourceEditor.tsx';
import type {SourceEditorHandle} from '../editor/SourceEditor.tsx';
import {CompletionClient} from '../editor/completion-client.ts';
import type {CompletionContext, CompletionKey, CompletionStatus} from '../editor/completion-types.ts';
import {AUTHORING_RECOVERY, catalogBlock, dirtyDrafts, draftList, fileDirty, findMatch, matchSummary, otherNonImageBytes, replacementEdit, saveBlock, shortRevision, validationCurrent} from '../authoring.ts';
import type {AuthoringSession, EditInput, FileDraft, Snapshot, TextRange, TypedText} from '../authoring.ts';
import {parseJson, readManifest, schemaIssues, treeKind} from '../metadata.ts';
import {packageDestination} from '../state.ts';
import type {AuthoringFileKind, CatalogEdit, EditorCompletionPreferences} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Metadata row order in the tree's Metadata group; the Files group holds sources and assets.
const METADATA_ORDER: Record<AuthoringFileKind, number> = {manifest: 0, schema: 1, profile: 2, source_map: 3, source: 4, asset: 5};

// The tree's two file groups. Selecting a row shows that file's editor, form or facts on the right.
type Group = 'files' | 'metadata';
// What a menu acts on. Every target names the row it was opened from; the selected file is never implied.
type MenuTarget = {kind: 'file' | 'metadata'; path: string};
interface OpenMenu {serial: number; target: MenuTarget; anchor: MenuAnchor; opener: HTMLElement | null; toggle: HTMLElement | null}

// Sources, assets and drafts of files no longer declared belong to Files; declared metadata belongs to Metadata.
function groupOf(draft: FileDraft): Group {
  return draft.missing || treeKind(draft.kind) ? 'files' : 'metadata';
}

function menuKeyOf(target: MenuTarget): string {
  return `${target.kind}:${target.path}`;
}

export interface EditHandlers {
  select: (path: string, previous: TextRange | null) => void;
  recognition: (previous: TextRange | null) => void;
  edit: (path: string, next: Snapshot, before: TextRange, input: EditInput) => void;
  compositionStart: (path: string, range: TextRange) => void;
  compositionEnd: (path: string) => void;
  range: (path: string, range: TextRange) => void;
  undo: (path: string) => void; redo: (path: string) => void;
  reveal: (path: string, range: TextRange) => void;
  // Structured metadata views replace a whole document; they keep no text history. A values form also passes the
  // provenance of its typed numeric text, recorded with the draft (see FileDraft.typed).
  replace: (path: string, text: string, typed?: TypedText[]) => void;
  discard: (path: string) => void;
  save: (path: string) => void; saveAll: () => void;
  validate: () => void;
  refresh: () => void; recover: () => void;
  // Resolves true once the host committed the edit, so the Add, Rename or Remove dialog can close.
  catalog: (edit: CatalogEdit) => Promise<boolean>;
  // The host places the copy in the configured packages root (the sources folder) under the new ID.
  duplicate: (packageId: string) => void;
  exit: () => void;
}

// Edit entry and ownership facts shown on one Tab's Guidance or Run page.
export interface PageAuthoring {
  // 'owner' when this Tab holds the application's Edit lease, 'other' when another Tab does.
  role: 'owner' | 'other' | null;
  ownerLabel: string | null;
  // Why Open/Create is unavailable: another owner, an unsettled operation, a pending command or closing.
  block: string | null;
  // A local editor view exists for the owner; without one, Return to Edit reloads the saved files from disk.
  loaded: boolean;
  onOpen: (path: string) => void; onCreate: () => void; onReturn: () => void;
  onPath: (value: string) => void; onPackageId: (value: string) => void;
  onRecover: (packagePath: string) => void;
}

interface Props {
  session: AuthoringSession; label: string; handlers: EditHandlers;
  recognition: ReactNode; recognitionDirty: boolean;
  // Why the dirty recognition draft cannot be saved as it is; Save all still saves file drafts first.
  recognitionSaveBlock: string | null;
  // The effective packages root (the sources folder) the host resolves Duplicate destinations in; shown as a preview only.
  packagesRoot: string;
  completionPreferences: EditorCompletionPreferences;
  // Another host command is in flight or the application is closing; typing stays available.
  locked: boolean; lockReason: string | null;
  // The host no longer reports this lease: text is kept for copying, publication is refused.
  leaseLost: boolean;
  // The validation child still holds the work reservation, as reported by the host controller.
  validationActive: boolean;
}

export default function EditPage({session, label, handlers, recognition, recognitionDirty, recognitionSaveBlock, packagesRoot, completionPreferences, locked, lockReason, leaseLost, validationActive}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const a = t.authoring;
  const drafts = draftList(session);
  const dirty = dirtyDrafts(session);
  const recognitionSelected = session.destination === 'recognition';
  const selected = recognitionSelected || session.selected === null ? undefined : session.drafts.get(session.selected);
  // Only sources get the text editor; declared metadata opens as structured views and assets as inventory facts.
  const text = selected !== undefined && selected.text !== null ? selected as FileDraft & {text: string} : undefined;
  const editable = text?.kind === 'source' ? text : undefined;
  const selection = useRef<TextRange>(selected?.range ?? {start: 0, end: 0});
  const searchInput = useRef<HTMLInputElement>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [replaceOpen, setReplaceOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [duplicateId, setDuplicateId] = useState('');
  const [replacement, setReplacement] = useState('');
  const [replacementNotice, setReplacementNotice] = useState<'stale' | 'ineligible' | 'oversized' | null>(null);
  const sourceEditor = useRef<SourceEditorHandle | null>(null);
  const client = useRef<CompletionClient | null>(null);
  const [completionStatus, setCompletionStatus] = useState<CompletionStatus>('idle');
  const schemaDraft = drafts.find(draft => draft.kind === 'schema' && !draft.missing);
  const schemaText = schemaDraft?.text ?? null;
  const optionsAvailable = useMemo(() => {
    if (schemaText === null) return false;
    const parsed = parseJson(schemaText);
    return parsed.ok && schemaIssues(parsed.value).length === 0;
  }, [schemaText]);
  const manifestText = drafts.find(draft => draft.kind === 'manifest' && !draft.missing)?.text ?? null;
  // Declarations name presets, maps and assets; the manifest view never changes them, so its draft is authoritative here.
  const manifest = useMemo(() => {
    const document = manifestText === null ? null : parseJson(manifestText);
    return document?.ok ? readManifest(document.value) : null;
  }, [manifestText]);

  // Tree disclosure is view state only. Selecting a file in a collapsed group (from a form, a diagnostic, Add or
  // Rename) opens the group in the same render, so the row is visible when the page reveals it.
  const selectedGroup = selected ? groupOf(selected) : null;
  const [groups, setGroups] = useState<Record<Group, boolean>>({files: true, metadata: true});
  const revealRequest = session.reveal;
  const revealKey = `${session.owner.token}:${revealRequest}:${session.selected ?? ''}`;
  const [revealedKey, setRevealedKey] = useState(revealKey);
  if (revealedKey !== revealKey) {
    setRevealedKey(revealKey);
    if (selectedGroup !== null && !groups[selectedGroup]) setGroups({...groups, [selectedGroup]: true});
  }
  // Host-driven reveals bring the file into sight on both sides; the editor focuses without scrolling the page.
  useEffect(() => {
    if (revealRequest === 0 || recognitionSelected) return;
    const main = document.getElementById('authoring-main');
    const top = main?.getBoundingClientRect().top ?? 0;
    const shell = main?.closest('.app');
    const clearance = shell ? parseFloat(getComputedStyle(shell).getPropertyValue('--navigation-clearance')) || 0 : 0;
    if (main && (top < clearance + 12 || top > clearance + (window.innerHeight - clearance) / 2)) main.scrollIntoView({block: 'start'});
    const body = document.getElementById('authoring-rail-body');
    const path = session.selected;
    const row = body && path !== null ? body.querySelector<HTMLElement>(`[data-path="${CSS.escape(path)}"]`) : null;
    if (!body || !row) return;
    const offset = row.getBoundingClientRect().top - body.getBoundingClientRect().top;
    if (offset < 0 || offset + row.offsetHeight > body.clientHeight) body.scrollTop += offset - body.clientHeight / 3;
  }, [session.owner.token, revealRequest, recognitionSelected]);

  const [railOpen, setRailOpen] = useState(true);
  const railToggle = useRef<HTMLButtonElement>(null);
  // The toggle moves between the tree heading and the file heading; keep focus on it across the move.
  const focusToggle = useRef(false);
  useLayoutEffect(() => {
    if (!focusToggle.current) return;
    focusToggle.current = false;
    railToggle.current?.focus();
  }, [railOpen]);
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const menuSerial = useRef(0);
  const [intent, setIntent] = useState<CatalogIntent | null>(null);
  const [duplicating, setDuplicating] = useState(false);
  const pending = session.pending;
  const validating = pending?.kind === 'validate' || validationActive;
  // Publication needs a live lease and no other host command; typing and navigation never wait for it.
  const publishLocked = locked || leaseLost;
  const saveReason = selected ? saveBlock(session, selected.path) : null;
  const savable = dirty.filter(draft => !draft.missing);
  const saveAllReason = pending !== null ? a.block('pending') : session.refreshRequired || session.conflict ? a.block('refresh') : null;
  const readOnly = pending?.kind === 'exit' || pending?.kind === 'duplicate';
  const summary = useMemo(() => editable && query ? matchSummary(editable.text, query, editable.range) : null,
    [editable?.text, query, editable?.range.start, editable?.range.end]);
  const sourceReadOnly = readOnly || leaseLost || editable?.missing === true;
  const generation = useRef(0);
  const otherBytes = useMemo(() => editable ? otherNonImageBytes(session, editable.path) : 0, [session.drafts, editable?.path]);
  const completionContext = useMemo<CompletionContext | null>(() => {
    generation.current += 1;
    if (!editable || sourceReadOnly || editable.composing !== null || pending?.kind === 'catalog'
      || pending?.kind === 'refresh' || session.refreshRequired) return null;
    return {owner: session.owner.token, path: editable.path, revision: editable.revision, generation: generation.current,
      source: editable.text, schema: schemaText, otherBytes};
  }, [session.owner.token, session.revision, session.order, session.destination, session.refreshRequired, session.conflict,
    pending?.kind, editable?.path, editable?.text, editable?.revision, editable?.composing, sourceReadOnly,
    schemaText, schemaDraft?.revision, otherBytes]);
  const currentContext = useRef(completionContext);
  currentContext.current = completionContext;
  const currentSession = useRef(session);
  currentSession.current = session;
  const currentReadOnly = useRef(sourceReadOnly);
  currentReadOnly.current = sourceReadOnly;

  useLayoutEffect(() => {
    currentContext.current = completionContext;
    client.current?.setContext(completionContext);
  }, [completionContext]);
  useLayoutEffect(() => {
    setReplacementNotice(null);
  }, [completionContext, query, replacement]);
  useLayoutEffect(() => {
    setCompletionStatus('idle');
    return () => {
      currentContext.current = null;
      client.current?.dispose();
      client.current = null;
    };
  }, [session.owner.token]);

  function openSearch() {
    flushSync(() => setSearchOpen(true));
    searchInput.current?.focus();
    searchInput.current?.select();
  }
  function closeSearch() {
    setSearchOpen(false);
    sourceEditor.current?.focus();
  }

  function invalidateCompletion(next?: Snapshot) {
    const context = currentContext.current;
    const updated = context && next ? {...context, source: next.text, revision: context.revision + 1, generation: ++generation.current} : null;
    currentContext.current = updated;
    client.current?.setContext(updated);
  }
  function acceptsCompletion(key: CompletionKey, source: string): boolean {
    const context = currentContext.current;
    const draft = currentSession.current.drafts.get(key.path);
    return !currentReadOnly.current && sourceEditor.current?.composing() !== true && context !== null
      && key.owner === context.owner && key.path === context.path && key.revision === context.revision
      && key.generation === context.generation && source === context.source && draft?.text === source
      && draft.revision === key.revision && draft.composing === null && client.current?.accepts(key) === true;
  }
  async function requestCompletion(position: number, explicit: boolean) {
    const context = currentContext.current;
    if (!context || currentReadOnly.current || sourceEditor.current?.composing()) return null;
    if (!client.current) client.current = new CompletionClient(setCompletionStatus);
    const active = client.current;
    active.setContext(context);
    const result = await active.request(position, explicit);
    return result && acceptsCompletion(result.key, context.source) ? result : null;
  }
  function replaceMatch(all: boolean) {
    if (!editable || sourceReadOnly || sourceEditor.current?.composing()) return;
    const ticket = {owner: session.owner.token, path: editable.path, revision: editable.revision};
    const before = selection.current;
    const result = replacementEdit(currentSession.current, ticket, query, replacement, all, before);
    setReplacementNotice(result.kind === 'refused' ? result.reason : null);
    if (result.kind === 'edit') {
      invalidateCompletion(result.next);
      handlers.edit(editable.path, result.next, before, {type: 'insertReplacementText', data: null, composing: false});
      handlers.reveal(editable.path, {start: result.next.start, end: result.next.end});
    } else if (result.kind === 'select') handlers.reveal(editable.path, result.range);
  }

  function find(backward: boolean) {
    if (!editable) return;
    const match = findMatch(editable.text, query, selection.current ?? editable.range, backward);
    if (match) handlers.reveal(editable.path, match);
  }
  // The outgoing selection matters only for the text editor, whose caret is restored when returning to the file.
  const previous = () => editable ? selection.current : null;
  const presetIds = new Map(manifest?.profiles.map(([id, path]): [string, string] => [path, id]) ?? []);
  const mapModules = new Map(manifest?.sourceMaps.map(([module, path]): [string, string] => [path, module]) ?? []);
  const metadata = drafts.filter(draft => !draft.missing && !treeKind(draft.kind)).sort((left, right) => METADATA_ORDER[left.kind] - METADATA_ORDER[right.kind]);
  function metadataLabel(draft: FileDraft): string {
    if (draft.kind === 'manifest') return a.metadataManifest;
    if (draft.kind === 'schema') return a.metadataSchema;
    if (draft.kind === 'profile') return a.presetLabel(presetIds.get(draft.path) ?? draft.path);
    return a.sourceMapLabel(mapModules.get(draft.path) ?? draft.path);
  }
  const missing = drafts.filter(draft => draft.missing);

  const duplicateDestination = packageDestination(packagesRoot, duplicateId.trim());
  const formDisabled = readOnly || selected?.missing === true;
  // Catalog edits and Duplicate follow the publication rules: a live lease and no other host command first.
  const publishReason = leaseLost ? a.leaseLostShort : locked ? lockReason ?? a.block('pending') : null;
  function catalogReason(edit: CatalogEdit): string | null {
    if (publishReason !== null) return publishReason;
    const block = catalogBlock(session, edit);
    return block === null ? null : a.block(block);
  }
  const addReason = catalogReason({kind: 'add', path: '', file_kind: 'source', text: ''});
  const duplicateReason = publishReason ?? (pending !== null || validating ? a.block('pending') : null);
  // The entry dialog closes before the host flow starts, so the unsaved-changes choice never stacks on top of it.
  function duplicate() {
    const id = duplicateId.trim();
    if (duplicateReason !== null || id === '') return;
    flushSync(() => setDuplicating(false));
    handlers.duplicate(id);
  }

  function openMenu(target: MenuTarget, anchor: MenuAnchor, opener: HTMLElement | null, trigger: boolean) {
    if (target.kind === 'metadata' && session.drafts.get(target.path)?.kind === 'manifest') {
      closeMenu(false);
      return;
    }
    if (trigger && menu !== null && menuKeyOf(menu.target) === menuKeyOf(target)) {
      closeMenu(true);
      return;
    }
    menuSerial.current += 1;
    setMenu({serial: menuSerial.current, target, anchor, opener, toggle: trigger ? opener : null});
  }
  function closeMenu(restoreFocus: boolean) {
    if (restoreFocus && menu?.opener?.isConnected) menu.opener.focus({preventScroll: true});
    setMenu(null);
  }
  // Rename and Remove act on the menu's own row; a row that disappeared meanwhile is refused, not substituted.
  function changeActions(path: string): MenuAction[] {
    const draft = session.drafts.get(path);
    const blocked = !draft || draft.missing ? a.targetGone : catalogReason({kind: 'remove', path});
    const actions: MenuAction[] = [
      {id: 'authoring-menu-rename', label: a.renameItem, blocked, onSelect: () => setIntent({kind: 'rename', path})},
    ];
    if (draft?.kind !== 'schema') {
      actions.push({id: 'authoring-menu-remove', label: a.remove, blocked, danger: true, onSelect: () => setIntent({kind: 'remove', path})});
    }
    return actions;
  }
  const openKey = menu === null ? null : menuKeyOf(menu.target);
  function menuTrigger(target: MenuTarget) {
    const key = menuKeyOf(target);
    return <button type="button" className="row-menu" aria-haspopup="menu" aria-expanded={openKey === key} aria-label={a.fileActions(target.path)} title={a.fileActions(target.path)}
      data-menu={key} onClick={event => openMenu(target, elementAnchor(event.currentTarget, event.detail === 0), event.currentTarget, true)}
      {...menuEvents((anchor, opener) => openMenu(target, anchor, opener, false))}><span aria-hidden="true">⋯</span></button>;
  }
  // After a dialog whose opener row was renamed or removed, focus the selected file's row, else the tree toggle.
  function catalogFocus(): HTMLElement | null {
    const path = session.selected;
    const row = path === null ? null : document.querySelector<HTMLElement>(`#authoring-rail [data-path="${CSS.escape(path)}"]`);
    return row && row.getClientRects().length > 0 ? row : railToggle.current;
  }
  function groupRow(group: Group, label: string) {
    const expanded = groups[group];
    // A collapsed group still reports unsaved and changed-on-disk files inside it.
    const unsaved = !expanded && dirty.some(draft => groupOf(draft) === group);
    const stale = !expanded && drafts.some(draft => draft.diskChanged && groupOf(draft) === group);
    return <div className="tree-row group-row">
      <button id={`authoring-group-${group}`} type="button" className="tree-folder tree-group" aria-expanded={expanded} aria-controls={`authoring-group-${group}-items`}
        onClick={() => setGroups(current => ({...current, [group]: !current[group]}))}>
        <span className="tree-twisty" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
        <span className="tree-name">{label}</span>
        {(unsaved || stale) && <span className="tree-meta">
          {unsaved && <span className="tag unsaved">{a.unsaved}</span>}
          {stale && <span className="tag stale">{a.diskChanged}</span>}</span>}
      </button>
    </div>;
  }
  const validation = session.validation;
  const current = validationCurrent(session);
  // A publication or a validation that reached disk capture can report an interrupted save that needs recovery.
  const recoveryFault = session.error?.category === AUTHORING_RECOVERY || (current && validation?.diagnostics.some(item => item.category === AUTHORING_RECOVERY) === true);

  const railButton = <button ref={railToggle} id="authoring-tree-toggle" type="button" className="icon-button" aria-controls="authoring-rail" aria-expanded={railOpen}
    aria-label={railOpen ? a.collapseTree : a.expandTree} title={railOpen ? a.collapseTree : a.expandTree}
    onClick={() => {focusToggle.current = true; setRailOpen(current => !current);}}><span className="rail-icon" aria-hidden="true"/></button>;
  // The selected file's editor, form or facts: always the file Save acts on.
  const fileView = selected && <>
    {selected.diskChanged && <p className="inline-warning">{a.diskChangedHelp}</p>}
    {selected.missing && <p className="inline-warning">{a.missingHelp}</p>}
    {text && (text.kind === 'manifest' || text.kind === 'schema' || text.kind === 'profile') && <div className="editor-toolbar">
      <ButtonHint hint={a.discardHint}>
        <button id="authoring-discard-file" type="button" className="danger-text" disabled={!fileDirty(text) || readOnly} onClick={() => handlers.discard(text.path)}>{a.discardFile}</button>
      </ButtonHint>
    </div>}
    {editable && <>
      <div className="editor-toolbar source-toolbar">
        <button id="authoring-undo" type="button" disabled={sourceReadOnly || editable.undo.length === 0 || editable.composing !== null} onClick={() => handlers.undo(editable.path)}>{a.undo}</button>
        <button id="authoring-redo" type="button" disabled={sourceReadOnly || editable.redo.length === 0 || editable.composing !== null} onClick={() => handlers.redo(editable.path)}>{a.redo}</button>
        <button id="authoring-discard-file" type="button" className="danger-text" disabled={!fileDirty(editable) || readOnly || editable.composing !== null} onClick={() => handlers.discard(editable.path)}>{a.discardFile}</button>
        <ButtonHint hint={a.completionHint}>
          <button id="authoring-complete" type="button" disabled={!completionContext} aria-keyshortcuts="Control+Space"
            onClick={() => sourceEditor.current?.complete()}>{a.complete}</button>
        </ButtonHint>
        <button id="authoring-find" type="button" aria-expanded={searchOpen} aria-controls="authoring-find-panel"
          onClick={() => searchOpen ? closeSearch() : openSearch()}>{a.find}</button>
      </div>
      <div className="source-editing-area">
      {searchOpen && <div id="authoring-find-panel" className="editor-find-panel" role="search" aria-label={a.search}
        onKeyDown={event => {
          if (event.key !== 'Escape' || event.defaultPrevented || event.nativeEvent.isComposing || event.keyCode === 229) return;
          event.preventDefault();
          event.stopPropagation();
          closeSearch();
        }}>
        <span className="editor-search">
          <label className="visually-hidden" htmlFor="authoring-search">{a.search}</label>
          <input id="authoring-search" ref={searchInput} type="search" value={query} spellCheck={false} placeholder={a.searchPlaceholder}
            onChange={event => setQuery(event.target.value)}
            onKeyDown={event => {
              if (event.nativeEvent.isComposing || event.keyCode === 229) return;
              if (event.key === 'Enter') {event.preventDefault(); find(event.shiftKey);}
            }}/>
          <button id="authoring-search-previous" type="button" disabled={!query} onClick={() => find(true)}>{a.previous}</button>
          <button id="authoring-search-next" type="button" disabled={!query} onClick={() => find(false)}>{a.next}</button>
          <span id="authoring-search-count" className="muted" role="status">{summary ? a.matches(summary.count, summary.current, summary.capped) : ''}</span>
          <button id="authoring-replace-toggle" className="editor-replace-toggle" type="button" aria-label={a.replace} title={a.replace}
            aria-controls="authoring-replacement-row" aria-expanded={replaceOpen} onClick={() => setReplaceOpen(current => !current)}>
            <svg width="18" height="18" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
              <text x="2" y="9" fill="currentColor" fontFamily="monospace" fontSize="11">a</text>
              <text x="10" y="19" fill="currentColor" fontFamily="monospace" fontSize="11">b</text>
              <path d="M11 4h5v7m-3-3 3 3 3-3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/>
            </svg>
          </button>
          <button id="authoring-find-close" type="button" aria-label={t.common.close} title={t.common.close} onClick={closeSearch}>×</button>
        </span>
        {replaceOpen && <span id="authoring-replacement-row" className="editor-replacement" role="group" aria-label={a.replacement}>
          <label className="visually-hidden" htmlFor="authoring-replacement">{a.replacement}</label>
          <input id="authoring-replacement" type="text" value={replacement} spellCheck={false} placeholder={a.replacementPlaceholder}
            onChange={event => setReplacement(event.target.value)}
            onKeyDown={event => {
              if (event.nativeEvent.isComposing || event.keyCode === 229) return;
              if (event.key === 'Enter') {event.preventDefault(); replaceMatch(false);}
            }}/>
          <button id="authoring-replace" type="button" disabled={!query || sourceReadOnly || editable.composing !== null} onClick={() => replaceMatch(false)}>{a.replace}</button>
          <button id="authoring-replace-all" type="button" disabled={!query || sourceReadOnly || editable.composing !== null} onClick={() => replaceMatch(true)}>{a.replaceAll}</button>
        </span>}
        <div className="editor-replacement-notice" role="status">{replacementNotice === null ? '' : a.replacementRefusals[replacementNotice]}</div>
      </div>}
      <SourceEditor key={`${session.owner.token}:${editable.path}`} draft={editable} reveal={session.reveal} readOnly={sourceReadOnly}
        selection={selection} control={sourceEditor} context={completionContext} request={requestCompletion} accepts={acceptsCompletion}
        completionPreferences={completionPreferences}
        completionUnavailable={completionStatus === 'unavailable' || completionStatus === 'oversized'}
        onCompletionRefused={() => setCompletionStatus('oversized')}
        onEdit={(next, before, input) => {
          invalidateCompletion(next);
          handlers.edit(editable.path, next, before, input);
        }}
        onRange={range => handlers.range(editable.path, range)}
        onCompositionStart={range => {invalidateCompletion(); handlers.compositionStart(editable.path, range);}}
        onCompositionEnd={() => handlers.compositionEnd(editable.path)}
        onUndo={() => handlers.undo(editable.path)} onRedo={() => handlers.redo(editable.path)} onSave={() => handlers.save(editable.path)}
        onFind={openSearch} onFindNext={find}/>
      </div>
      <div className="completion-status">
        <span>{a.completionList}<HelpTrigger title={a.completionList} hint={a.completionHint}><p>{a.completionScope}</p></HelpTrigger></span>
        <span role="status"><span id="authoring-completion-status">{a.completionStatuses[completionStatus]}</span>
          {!optionsAvailable && <> <span id="authoring-options-unavailable">{a.optionsCompletionUnavailable}</span></>}</span>
      </div>
    </>}
    {text?.kind === 'manifest' && <ManifestEditor key={text.path} draft={text} disabled={formDisabled}
      onReplace={next => handlers.replace(text.path, next)} onOpen={path => handlers.select(path, null)}/>}
    {text?.kind === 'schema' && <SchemaEditor key={text.path} draft={text} disabled={formDisabled} onReplace={(next, typed) => handlers.replace(text.path, next, typed)}/>}
    {text?.kind === 'profile' && <PresetEditor key={text.path} draft={text} presetId={presetIds.get(text.path) ?? null} packageId={session.packageId}
      schema={drafts.find(draft => draft.kind === 'schema' && !draft.missing)} disabled={formDisabled}
      onReplace={(next, typed) => handlers.replace(text.path, next, typed)} onOpen={path => handlers.select(path, null)}/>}
    {text?.kind === 'source_map' && <SourceMapView key={text.path} draft={text} module={mapModules.get(text.path) ?? null}/>}
    {selected.kind === 'asset' && <AssetView key={selected.path} draft={selected} asset={manifest?.assets.find(asset => asset.path === selected.path) ?? null}/>}
  </>;

  return <>
    <div className="page-heading"><div><span className="eyebrow">{a.scope(label)}</span><h1 aria-labelledby="authoring-heading"><span id="authoring-heading">{session.packageId}</span>
      <HelpTrigger title={a.scope(label)} hint={a.introHint}><p>{a.intro}</p><p>{a.authority}</p></HelpTrigger></h1></div>
      <div className="actions">
        <button id="authoring-save" type="button" className="primary" disabled={publishLocked || !selected || saveReason !== null}
          title={saveReason ? a.block(saveReason) : undefined} onClick={() => selected && handlers.save(selected.path)}>{pending?.kind === 'save' ? a.working : a.save}</button>
        <button id="authoring-save-all" type="button" disabled={publishLocked || (savable.length === 0 && (!recognitionDirty || recognitionSaveBlock !== null)) || saveAllReason !== null}
          title={saveAllReason ?? recognitionSaveBlock ?? undefined} onClick={handlers.saveAll}>{a.saveAll}</button>
        <ButtonHint hint={a.validationHelp}>
          <button id="authoring-validate" type="button" disabled={publishLocked || pending !== null || validating} onClick={handlers.validate}>{validating ? a.validating : a.validate}</button>
        </ButtonHint>
        <ButtonHint hint={a.exitHint}>
          <button id="authoring-exit" type="button" disabled={locked || (pending !== null && pending.kind !== 'recognition_trial' && pending.kind !== 'validate')}
            onClick={handlers.exit}>{pending?.kind === 'exit' ? a.working : a.exit}</button>
        </ButtonHint>
      </div>
    </div>
    {locked && lockReason && <p id="authoring-lock-reason" className="operation-status" role="status">{lockReason}</p>}
    {leaseLost && <p id="authoring-lease-lost" className="inline-warning" role="alert">{a.leaseLost}</p>}
    {session.error && <div id="authoring-error"><FaultMessage title={a.actionFailed} value={session.error}/></div>}
    {recoveryFault && <div className="button-row"><button id="authoring-recover" type="button" disabled={locked} onClick={handlers.recover}>{a.recover}</button>
      <span className="muted">{a.recoverHelp}</span></div>}
    {session.conflict && <section id="authoring-conflict" className="inline-warning" role="alert"><strong>{a.conflictHeading}</strong> {a.conflictHelp}
      <button id="authoring-refresh-conflict" type="button" disabled={locked || pending !== null} onClick={handlers.refresh}>{a.refresh}</button></section>}
    {session.refreshError && <div id="authoring-refresh-error"><FaultMessage title={a.refreshFailed} value={session.refreshError}/>
      {session.refreshRequired && <p className="inline-warning">{a.refreshRequired}</p>}
      <div className="button-row"><button id="authoring-refresh-retry" type="button" disabled={locked || pending !== null} onClick={handlers.refresh}>{a.refresh}</button></div></div>}
    <div className="repo-facts">
      <dl>
        <div><dt>{a.path}</dt><dd id="authoring-package-path" className="mono">{session.packagePath}</dd></div>
        <div><dt>{a.revision}</dt><dd id="authoring-revision" className="mono" title={session.revision}>{shortRevision(session.revision)}</dd></div>
      </dl>
    </div>
    <div className={railOpen ? 'repo' : 'repo rail-closed'}>
      <aside id="authoring-rail" className="panel repo-rail" aria-labelledby="authoring-rail-heading" hidden={!railOpen} onContextMenu={event => event.preventDefault()}>
        <div className="rail-heading"><h2 aria-labelledby="authoring-rail-heading"><span id="authoring-rail-heading">{a.railHeading}</span>
          <HelpTrigger title={a.railHeading} hint={a.filesHint}><p>{a.filesHelp}</p></HelpTrigger></h2>
          <button id="authoring-tree-add" type="button" className="icon-button" aria-label={a.addFile} title={addReason ?? a.addFile}
            disabled={addReason !== null} onClick={() => setIntent({kind: 'add'})}><span aria-hidden="true">+</span></button>
          {railOpen && railButton}</div>
        <div id="authoring-rail-body" className="rail-body">
          <ul className="file-tree rail-groups">
            <li>
              {groupRow('files', a.files)}
              <div id="authoring-group-files-items" className="tree-children" hidden={!groups.files}>
                <FileTree drafts={drafts.filter(draft => !draft.missing)} selected={recognitionSelected ? null : session.selected} reveal={revealRequest}
                  onSelect={path => handlers.select(path, previous())} onMenu={(path, anchor, opener, trigger) => openMenu({kind: 'file', path}, anchor, opener, trigger)} menuOpen={openKey}/>
                {missing.length > 0 && <section className="rail-missing" aria-labelledby="authoring-missing-heading">
                  <h3 id="authoring-missing-heading">{a.missingHeading}</h3><p className="field-help">{a.missingHelp}</p>
                  <ul id="authoring-missing" className="file-tree">{missing.map(draft => <li key={draft.path}>
                    <div className={!recognitionSelected && draft.path === session.selected ? 'tree-row current' : 'tree-row'}>
                      <button type="button" className="tree-file" data-path={draft.path} title={draft.path} aria-current={!recognitionSelected && draft.path === session.selected ? 'true' : undefined}
                        onClick={() => handlers.select(draft.path, previous())}><span className="tree-name mono">{draft.path}</span>
                        <span className="tree-meta"><span className="tag">{a.kind(draft.kind)}</span><span className="tag stale">{a.unsaved}</span></span></button>
                    </div></li>)}</ul>
                </section>}
              </div>
            </li>
            <li>
              {groupRow('metadata', a.metadataHeading)}
              <ul id="authoring-group-metadata-items" className="file-tree tree-children" hidden={!groups.metadata}>
                {metadata.map(draft => {
                  const target: MenuTarget = {kind: 'metadata', path: draft.path};
                  const current = !recognitionSelected && draft.path === session.selected;
                  return <li key={draft.path}><div className={current ? 'tree-row current' : 'tree-row'}>
                    <button type="button" className="tree-file" data-path={draft.path} data-kind={draft.kind} title={draft.path} aria-current={current ? 'true' : undefined}
                      aria-keyshortcuts={draft.kind === 'manifest' ? undefined : 'Shift+F10'} onClick={() => handlers.select(draft.path, previous())}
                      {...menuEvents((anchor, opener) => openMenu(target, anchor, opener, false))}>
                      <span className="tree-name">{metadataLabel(draft)}</span>
                      {(fileDirty(draft) || draft.diskChanged) && <span className="tree-meta">
                        {fileDirty(draft) && <span className="tag unsaved">{a.unsaved}</span>}
                        {draft.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
                    </button>
                    {draft.kind !== 'manifest' && menuTrigger(target)}
                  </div></li>;
                })}
              </ul>
            </li>
            <li><div className={recognitionSelected ? 'tree-row current' : 'tree-row'}>
              <button id="authoring-recognition" type="button" className="tree-folder tree-action" aria-current={recognitionSelected ? 'true' : undefined}
                onClick={() => handlers.recognition(previous())}>
                <svg className="recognition-icon" viewBox="0 0 12 12" aria-hidden="true"><path d="M1 4V1h3M8 1h3v3M11 8v3H8M4 11H1V8M4.5 4.5h3v3h-3z"/></svg>
                <span className="tree-name">{a.recognition}</span>
                {recognitionDirty && <span className="tree-meta"><span className="tag unsaved">{a.unsaved}</span></span>}</button>
            </div></li>
            <li><div className="tree-row">
              <button id="authoring-duplicate-open" type="button" className="tree-folder tree-action" aria-haspopup="dialog" onClick={() => setDuplicating(true)}>
                <span className="copy-icon" aria-hidden="true"/><span className="tree-name">{a.duplicateOpen}</span></button>
            </div></li>
          </ul>
        </div>
      </aside>
      <section id="authoring-main" className="panel repo-main" aria-labelledby="authoring-file-heading">
        <div className="main-bar">
          {!railOpen && railButton}
          {selected
            ? <div className="file-title"><span className="eyebrow">{`${selectedGroup === 'files' ? a.files : a.metadataHeading} · ${a.kind(selected.kind)}`}</span>
              <h2 id="authoring-file-heading" className="file-crumbs mono">{selected.path.split('/').map((part, index, parts) => <Fragment key={index}>
                {index > 0 && <span className="crumb-separator">/</span>}{index === parts.length - 1 ? <strong>{part}</strong> : part}</Fragment>)}</h2></div>
            : <h2 id="authoring-file-heading" className="file-title">{recognitionSelected ? a.recognition : a.noFile}</h2>}
          {selected && (fileDirty(selected) || selected.diskChanged) && <span className="tree-meta">
            {fileDirty(selected) && <span id="authoring-file-dirty" className="tag unsaved">{a.unsaved}</span>}
            {selected.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
        </div>
        {selected && <div className="repo-body">{fileView}</div>}
        {recognitionSelected && <div className="repo-body">{recognition}</div>}
      </section>
    </div>
    {menu && <ContextMenu key={menu.serial} id="authoring-menu" label={a.fileActions(menu.target.path)} anchor={menu.anchor} actions={changeActions(menu.target.path)}
      heading={menu.target.path} toggle={menu.toggle} onClose={closeMenu}/>}
    <CatalogDialog intent={intent} session={session} reason={catalogReason} onSubmit={handlers.catalog} onClose={() => setIntent(null)} returnFocus={catalogFocus}/>
    <Modal id="authoring-duplicate-dialog" open={duplicating} onCancel={() => setDuplicating(false)} labelledBy="authoring-duplicate-heading" className="catalog-dialog"
      initialFocus="#authoring-duplicate-id" returnFocus={() => document.getElementById('authoring-duplicate-open') ?? railToggle.current}>
      <form onSubmit={event => {event.preventDefault(); duplicate();}}>
        <div className="dialog-header"><div><h2 id="authoring-duplicate-heading">{a.duplicateHeading}</h2><p>{a.duplicateHelp}</p></div></div>
        <div className="dialog-body">
          <div className="field"><label htmlFor="authoring-duplicate-id">{a.newPackageId}</label>
            <input id="authoring-duplicate-id" type="text" value={duplicateId} spellCheck={false} autoCapitalize="off" autoCorrect="off"
              aria-describedby="authoring-duplicate-destination" onChange={event => setDuplicateId(event.target.value)}/>
            <p id="authoring-duplicate-destination" className="field-help">{duplicateDestination === null
              ? a.duplicateDestinationPending : <>{a.duplicateDestination} <span className="mono">{duplicateDestination}</span></>}</p></div>
          <div className="dialog-footer">
            <span id="authoring-duplicate-status" role="status">{duplicateReason ?? ''}</span>
            <button id="authoring-duplicate-cancel" type="button" onClick={() => setDuplicating(false)}>{a.cancel}</button>
            <button id="authoring-duplicate-submit" type="submit" className="primary" disabled={duplicateReason !== null || !duplicateId.trim()}>{a.duplicate}</button>
          </div>
        </div>
      </form>
    </Modal>
  </>;
}

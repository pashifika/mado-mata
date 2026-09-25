import {Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState} from 'react';
import type {MouseEvent, RefObject} from 'react';
import {flushSync} from 'react-dom';
import CatalogDialog from '../components/CatalogDialog.tsx';
import type {CatalogIntent} from '../components/CatalogDialog.tsx';
import ContextMenu, {elementAnchor, menuEvents, pointAnchor} from '../components/ContextMenu.tsx';
import type {MenuAction, MenuAnchor} from '../components/ContextMenu.tsx';
import FileTree from '../components/FileTree.tsx';
import type {TreeTarget} from '../components/FileTree.tsx';
import ManifestEditor from '../components/ManifestEditor.tsx';
import Modal from '../components/Modal.tsx';
import {AssetView, SourceMapView} from '../components/MetadataFacts.tsx';
import PresetEditor from '../components/PresetEditor.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import SchemaEditor from '../components/SchemaEditor.tsx';
import {AUTHORING_RECOVERY, catalogBlock, diagnosticLocation, dirtyDrafts, draftList, fileDirty, findMatch, lineColumn, lineCount, matchSummary, offsetAt, saveBlock, shortRevision, validationCurrent} from '../authoring.ts';
import type {AuthoringSession, EditInput, FileDraft, Snapshot, TextRange, TypedText} from '../authoring.ts';
import {parseJson, readManifest, treeKind} from '../metadata.ts';
import {packageDestination} from '../state.ts';
import type {AuthoringFileKind, CatalogAddKind, CatalogEdit, Fault} from '../types.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// One editor line in CSS pixels; `.code-editor` in style.css uses the same line height for text and gutter.
const LINE_HEIGHT = 18;
const INDENT = '  ';
// Metadata row order in the tree's Metadata group; the Files group holds sources and assets.
const METADATA_ORDER: Record<AuthoringFileKind, number> = {manifest: 0, schema: 1, profile: 2, source_map: 3, source: 4, asset: 5};

// The tree's two file groups. Selecting a row shows that file's editor, form or facts on the right.
type Group = 'files' | 'metadata';
// What a menu acts on. Every target names the row it was opened from; the selected file is never implied.
type MenuTarget = TreeTarget | {kind: 'files'} | {kind: 'metadata'; path: string} | {kind: 'metadataGroup'} | {kind: 'rail'};
interface OpenMenu {serial: number; target: MenuTarget; anchor: MenuAnchor; opener: HTMLElement | null; toggle: HTMLElement | null}

// Sources, assets and drafts of files no longer declared belong to Files; declared metadata belongs to Metadata.
function groupOf(draft: FileDraft): Group {
  return draft.missing || treeKind(draft.kind) ? 'files' : 'metadata';
}

// The folder part of a package path with its trailing slash, or '' for a file at the package root.
function folderOf(path: string): string {
  return path.slice(0, path.lastIndexOf('/') + 1);
}

function menuKeyOf(target: MenuTarget): string {
  return 'path' in target ? `${target.kind}:${target.path}` : target.kind;
}

export interface EditHandlers {
  select: (path: string, previous: TextRange | null) => void;
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
  validate: () => void; stopValidation: () => void;
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
  // The effective packages root (the sources folder) the host resolves Duplicate destinations in; shown as a preview only.
  packagesRoot: string;
  // Another host command is in flight or the application is closing; typing stays available.
  locked: boolean; lockReason: string | null;
  // The host no longer reports this lease: text is kept for copying, publication is refused.
  leaseLost: boolean;
  // The validation child still holds the work reservation, as reported by the host controller.
  validationActive: boolean;
}

interface EditorProps {
  draft: FileDraft & {text: string}; reveal: number; readOnly: boolean; label: string; help: string;
  // The latest editor selection, shared with the page for search and file switching without re-rendering it.
  selection: RefObject<TextRange>;
  handlers: EditHandlers; onFind: () => void; onFindNext: (backward: boolean) => void;
}

// A native textarea keeps platform text input, selection and IME composition. History is kept per file by the
// session instead of the element, so switching files never mixes undo stacks; native history commands are redirected.
function CodeEditor({draft, reveal, readOnly, label, help, selection, handlers, onFind, onFindNext}: EditorProps) {
  const locale = useLocale();
  const a = messages[locale].ui.authoring;
  const area = useRef<HTMLTextAreaElement>(null);
  const gutter = useRef<HTMLPreElement>(null);
  const composing = useRef(false);
  // Selection to restore after a programmatic text change (Tab insertion) is committed.
  const restore = useRef<TextRange | null>(null);
  const latest = useRef({handlers, path: draft.path});
  latest.current = {handlers, path: draft.path};
  const [caret, setCaret] = useState(() => lineColumn(draft.text, draft.range.start));
  const lines = lineCount(draft.text);
  const numbers = useMemo(() => Array.from({length: lines}, (_, index) => index + 1).join('\n'), [lines]);

  // Explicit reveals (file switch, undo/redo, search, diagnostics) apply the session's stored selection.
  useLayoutEffect(() => {
    const element = area.current;
    if (!element) return;
    const {start, end} = draft.range;
    element.focus({preventScroll: true});
    element.setSelectionRange(start, end);
    selection.current = {start, end};
    setCaret(lineColumn(element.value, start));
    const top = (lineColumn(element.value, start).line - 1) * LINE_HEIGHT;
    if (top < element.scrollTop || top > element.scrollTop + element.clientHeight - 2 * LINE_HEIGHT) {
      element.scrollTop = Math.max(0, top - element.clientHeight / 3);
    }
    if (gutter.current) gutter.current.scrollTop = element.scrollTop;
  }, [reveal, draft.path]);

  useLayoutEffect(() => {
    const element = area.current;
    const target = restore.current;
    if (!element || !target) return;
    restore.current = null;
    element.setSelectionRange(target.start, target.end);
    selection.current = target;
  });

  // Edit-menu Undo/Redo reach the element as native history input; they use this file's session history instead.
  useEffect(() => {
    const element = area.current;
    if (!element) return;
    function history(event: InputEvent) {
      if (event.inputType !== 'historyUndo' && event.inputType !== 'historyRedo') return;
      event.preventDefault();
      if (composing.current) return;
      const {handlers: current, path} = latest.current;
      if (event.inputType === 'historyUndo') current.undo(path); else current.redo(path);
    }
    element.addEventListener('beforeinput', history);
    return () => element.removeEventListener('beforeinput', history);
  }, []);

  function track(element: HTMLTextAreaElement) {
    selection.current = {start: element.selectionStart, end: element.selectionEnd};
    setCaret(lineColumn(element.value, element.selectionStart));
  }

  return <div className="code-editor">
    <pre id="authoring-line-numbers" ref={gutter} className="editor-gutter" aria-hidden="true">{numbers}</pre>
    <textarea id="authoring-editor" ref={area} className="editor-text" value={draft.text} readOnly={readOnly} wrap="off"
      spellCheck={false} autoCapitalize="off" autoCorrect="off" autoComplete="off" aria-label={label} aria-describedby="authoring-editor-help"
      data-path={draft.path} data-draft-revision={draft.revision}
      onChange={event => {
        const element = event.currentTarget;
        const native = event.nativeEvent as Partial<InputEvent>;
        const before = selection.current ?? {start: element.selectionStart, end: element.selectionEnd};
        const next = {text: element.value, start: element.selectionStart, end: element.selectionEnd};
        track(element);
        handlers.edit(draft.path, next, before, {type: native.inputType ?? '', data: native.data ?? null, composing: composing.current || native.isComposing === true});
      }}
      onSelect={event => track(event.currentTarget)}
      onBlur={event => handlers.range(draft.path, {start: event.currentTarget.selectionStart, end: event.currentTarget.selectionEnd})}
      onScroll={event => {if (gutter.current) gutter.current.scrollTop = event.currentTarget.scrollTop;}}
      onCompositionStart={event => {
        composing.current = true;
        handlers.compositionStart(draft.path, {start: event.currentTarget.selectionStart, end: event.currentTarget.selectionEnd});
      }}
      onCompositionEnd={() => {
        composing.current = false;
        handlers.compositionEnd(draft.path);
      }}
      onKeyDown={event => {
        if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
        const modifier = event.metaKey || event.ctrlKey;
        const key = event.key.toLowerCase();
        if (modifier && !event.altKey && (key === 'z' || key === 'y')) {
          event.preventDefault();
          if (key === 'y' || event.shiftKey) handlers.redo(draft.path); else handlers.undo(draft.path);
        } else if (modifier && key === 's') {
          event.preventDefault();
          handlers.save(draft.path);
        } else if (modifier && key === 'f') {
          event.preventDefault();
          onFind();
        } else if (modifier && key === 'g') {
          event.preventDefault();
          onFindNext(event.shiftKey);
        } else if (event.key === 'Tab' && !modifier && !event.altKey && !event.shiftKey && !readOnly) {
          event.preventDefault();
          const element = event.currentTarget;
          const {selectionStart: start, selectionEnd: end, value} = element;
          const caretAt = start + INDENT.length;
          restore.current = {start: caretAt, end: caretAt};
          handlers.edit(draft.path, {text: value.slice(0, start) + INDENT + value.slice(end), start: caretAt, end: caretAt}, {start, end},
            {type: 'insertText', data: INDENT, composing: false});
        }
      }}/>
    <div className="editor-status"><span id="authoring-caret">{a.position(caret.line, caret.column)}</span><span>{a.lines(lines)}</span>
      <span id="authoring-editor-help">{help}</span></div>
  </div>;
}

export default function EditPage({session, label, handlers, packagesRoot, locked, lockReason, leaseLost, validationActive}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const a = t.authoring;
  const drafts = draftList(session);
  const dirty = dirtyDrafts(session);
  const selected = session.selected === null ? undefined : session.drafts.get(session.selected);
  // Only sources get the text editor; declared metadata opens as structured views and assets as inventory facts.
  const text = selected !== undefined && selected.text !== null ? selected as FileDraft & {text: string} : undefined;
  const editable = text?.kind === 'source' ? text : undefined;
  const selection = useRef<TextRange>(selected?.range ?? {start: 0, end: 0});
  const searchInput = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [duplicateId, setDuplicateId] = useState('');
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
  const [revealRequest, setRevealRequest] = useState(0);
  const revealKey = `${revealRequest}:${session.selected ?? ''}`;
  const [revealedKey, setRevealedKey] = useState(revealKey);
  if (revealedKey !== revealKey) {
    setRevealedKey(revealKey);
    if (selectedGroup !== null && !groups[selectedGroup]) setGroups({...groups, [selectedGroup]: true});
  }
  // A diagnostic brings the file into sight on both sides; the editor itself focuses without scrolling the page.
  useEffect(() => {
    if (revealRequest === 0) return;
    const main = document.getElementById('authoring-main');
    const top = main?.getBoundingClientRect().top ?? 0;
    if (main && (top < 0 || top > window.innerHeight / 2)) main.scrollIntoView({block: 'start'});
    const body = document.getElementById('authoring-rail-body');
    const path = session.selected;
    const row = body && path !== null ? body.querySelector<HTMLElement>(`[data-path="${CSS.escape(path)}"]`) : null;
    if (!body || !row) return;
    const offset = row.getBoundingClientRect().top - body.getBoundingClientRect().top;
    if (offset < 0 || offset + row.offsetHeight > body.clientHeight) body.scrollTop += offset - body.clientHeight / 3;
  }, [revealRequest]);

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

  function find(backward: boolean) {
    if (!editable) return;
    const match = findMatch(editable.text, query, selection.current ?? editable.range, backward);
    if (match) handlers.reveal(editable.path, match);
  }
  // The outgoing selection matters only for the text editor, whose caret is restored when returning to the file.
  const previous = () => editable ? selection.current : null;
  function goTo(item: Fault) {
    const location = diagnosticLocation(item);
    const target = location ? session.drafts.get(location.path) : undefined;
    if (!location || !target) return;
    setRevealRequest(count => count + 1);
    if (target.kind !== 'source' || target.text === null) {
      handlers.select(location.path, previous());
      return;
    }
    const offset = offsetAt(target.text, location.line, location.column);
    handlers.reveal(location.path, {start: offset, end: offset});
  }
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
  const presetFolder = folderOf(manifest?.profiles[0]?.[1] ?? '');
  const mapFolder = folderOf(manifest?.sourceMaps[0]?.[1] ?? '');
  function add(fileKind: CatalogAddKind, prefix: string) {
    setIntent({kind: 'add', fileKind, prefix});
  }
  // The entry dialog closes before the host flow starts, so the unsaved-changes choice never stacks on top of it.
  function duplicate() {
    const id = duplicateId.trim();
    if (duplicateReason !== null || id === '') return;
    flushSync(() => setDuplicating(false));
    handlers.duplicate(id);
  }

  function openMenu(target: MenuTarget, anchor: MenuAnchor, opener: HTMLElement | null, trigger: boolean) {
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
  // Right-clicking a group or the tree's blank space opens that area's menu; focus returns to `opener` afterwards.
  function contextAt(target: MenuTarget, opener: string) {
    return (event: MouseEvent<HTMLElement>) => {
      event.preventDefault();
      event.stopPropagation();
      openMenu(target, pointAnchor(event.clientX, event.clientY), document.getElementById(opener), false);
    };
  }
  // Rename and Remove act on the menu's own row; a row that disappeared meanwhile is refused, not substituted.
  function changeActions(path: string, separated: boolean): MenuAction[] {
    const draft = session.drafts.get(path);
    const blocked = !draft || draft.missing ? a.targetGone : catalogReason({kind: 'remove', path});
    return [
      {id: 'authoring-menu-rename', label: a.renameItem, blocked, separated, onSelect: () => setIntent({kind: 'rename', path})},
      {id: 'authoring-menu-remove', label: a.remove, blocked, danger: true, onSelect: () => setIntent({kind: 'remove', path})},
    ];
  }
  function menuActions(target: MenuTarget): MenuAction[] {
    const addAction = (id: string, label: string, fileKind: CatalogAddKind, prefix: string, separated = false): MenuAction =>
      ({id, label, blocked: addReason, separated, onSelect: () => add(fileKind, prefix)});
    const metadataAdds = (separated: boolean) => [addAction('authoring-menu-add-preset', a.addPreset, 'profile', presetFolder, separated),
      addAction('authoring-menu-add-source-map', a.addSourceMap, 'source_map', mapFolder)];
    if (target.kind === 'file') {
      const folder = folderOf(target.path);
      return [addAction('authoring-menu-add', folder ? a.addIn(folder) : a.addFile, 'source', folder), ...changeActions(target.path, true)];
    }
    if (target.kind === 'folder') return [addAction('authoring-menu-add', a.addIn(`${target.path}/`), 'source', `${target.path}/`)];
    if (target.kind === 'files') return [addAction('authoring-menu-add', a.addFile, 'source', '')];
    if (target.kind === 'metadata' && session.drafts.get(target.path)?.kind !== 'manifest') return [...changeActions(target.path, false), ...metadataAdds(true)];
    if (target.kind !== 'rail') return metadataAdds(false);
    return [addAction('authoring-menu-add', a.addFile, 'source', ''), ...metadataAdds(false),
      {id: 'authoring-menu-duplicate', label: a.duplicateOpen, separated: true, onSelect: () => setDuplicating(true)}];
  }
  function menuLabel(target: MenuTarget): string {
    if (target.kind === 'files') return a.treeActions;
    if (target.kind === 'metadataGroup') return a.metadataActions;
    if (target.kind === 'rail') return a.railActions;
    return a.fileActions(target.kind === 'folder' ? `${target.path}/` : target.path);
  }
  const openKey = menu === null ? null : menuKeyOf(menu.target);
  function menuTrigger(target: MenuTarget) {
    const key = menuKeyOf(target);
    return <button type="button" className="row-menu" aria-haspopup="menu" aria-expanded={openKey === key} aria-label={menuLabel(target)} title={menuLabel(target)}
      data-menu={key} onClick={event => openMenu(target, elementAnchor(event.currentTarget), event.currentTarget, true)}
      {...menuEvents((anchor, opener) => openMenu(target, anchor, opener, false))}><span aria-hidden="true">⋯</span></button>;
  }
  // After a dialog whose opener row was renamed or removed, focus the selected file's row, else the tree toggle.
  function catalogFocus(): HTMLElement | null {
    const path = session.selected;
    const row = path === null ? null : document.querySelector<HTMLElement>(`#authoring-rail [data-path="${CSS.escape(path)}"]`);
    return row && row.getClientRects().length > 0 ? row : railToggle.current;
  }
  function groupRow(group: Group, label: string, target: MenuTarget) {
    const expanded = groups[group];
    // A collapsed group still reports unsaved and changed-on-disk files inside it.
    const unsaved = !expanded && dirty.some(draft => groupOf(draft) === group);
    const stale = !expanded && drafts.some(draft => draft.diskChanged && groupOf(draft) === group);
    return <div className="tree-row group-row">
      <button id={`authoring-group-${group}`} type="button" className="tree-folder tree-group" aria-expanded={expanded} aria-controls={`authoring-group-${group}-items`}
        aria-keyshortcuts="Shift+F10" onClick={() => setGroups(current => ({...current, [group]: !current[group]}))}
        {...menuEvents((anchor, opener) => openMenu(target, anchor, opener, false))}>
        <span className="tree-twisty" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
        <span className="tree-name">{label}</span>
        {(unsaved || stale) && <span className="tree-meta">
          {unsaved && <span className="tag unsaved">{a.unsaved}</span>}
          {stale && <span className="tag stale">{a.diskChanged}</span>}</span>}
      </button>
      {menuTrigger(target)}
    </div>;
  }
  const validation = session.validation;
  const current = validationCurrent(session);
  const statusText = renderMessage(locale, session.notice) || (lockReason ?? '');
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
      <button id="authoring-discard-file" type="button" className="danger-text" disabled={!fileDirty(text) || readOnly} onClick={() => handlers.discard(text.path)}>{a.discardFile}</button>
      <span className="muted">{a.structuredHelp}</span>
    </div>}
    {editable && <>
      <div className="editor-toolbar">
        <button id="authoring-undo" type="button" disabled={editable.undo.length === 0 || editable.composing !== null} onClick={() => handlers.undo(editable.path)}>{a.undo}</button>
        <button id="authoring-redo" type="button" disabled={editable.redo.length === 0 || editable.composing !== null} onClick={() => handlers.redo(editable.path)}>{a.redo}</button>
        <button id="authoring-discard-file" type="button" className="danger-text" disabled={!fileDirty(editable) || readOnly} onClick={() => handlers.discard(editable.path)}>{a.discardFile}</button>
        <span className="editor-search" role="search">
          <label className="visually-hidden" htmlFor="authoring-search">{a.search}</label>
          <input id="authoring-search" ref={searchInput} type="search" value={query} spellCheck={false} placeholder={a.searchPlaceholder}
            onChange={event => setQuery(event.target.value)}
            onKeyDown={event => {if (event.key === 'Enter') {event.preventDefault(); find(event.shiftKey);}}}/>
          <button id="authoring-search-previous" type="button" disabled={!query} onClick={() => find(true)}>{a.previous}</button>
          <button id="authoring-search-next" type="button" disabled={!query} onClick={() => find(false)}>{a.next}</button>
          <span id="authoring-search-count" className="muted" role="status">{summary ? a.matches(summary.count, summary.current, summary.capped) : ''}</span>
        </span>
      </div>
      <CodeEditor key={editable.path} draft={editable} reveal={session.reveal} readOnly={readOnly} label={a.editorLabel(editable.path)} help={a.editorHelp}
        selection={selection} handlers={handlers} onFind={() => {searchInput.current?.focus(); searchInput.current?.select();}} onFindNext={find}/>
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
    <div className="page-heading"><div><span className="eyebrow">{a.scope(label)}</span><h1 id="authoring-heading">{session.packageId}</h1>
      <p>{a.intro}</p></div>
      <div className="actions">
        <button id="authoring-save" type="button" className="primary" disabled={publishLocked || !selected || saveReason !== null}
          title={saveReason ? a.block(saveReason) : undefined} onClick={() => selected && handlers.save(selected.path)}>{pending?.kind === 'save' ? a.working : a.save}</button>
        <button id="authoring-save-all" type="button" disabled={publishLocked || savable.length === 0 || saveAllReason !== null} title={saveAllReason ?? undefined} onClick={handlers.saveAll}>{a.saveAll}</button>
        <button id="authoring-validate" type="button" disabled={publishLocked || pending !== null || validating} onClick={handlers.validate}>{validating ? a.validating : a.validate}</button>
        <button id="authoring-exit" type="button" disabled={locked || pending !== null || validating} title={a.exitHelp} onClick={handlers.exit}>{pending?.kind === 'exit' ? a.working : a.exit}</button>
      </div>
    </div>
    <div id="authoring-status" className="operation-status" role="status">{statusText}</div>
    <p className="authority-note">{a.authority}</p>
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
      {dirty.length > 0 && <span id="authoring-unsaved-count" className="tag unsaved">{a.unsavedFiles(dirty.length)}</span>}
    </div>
    <div className={railOpen ? 'repo' : 'repo rail-closed'}>
      <aside id="authoring-rail" className="panel repo-rail" aria-labelledby="authoring-rail-heading" hidden={!railOpen} onContextMenu={contextAt({kind: 'rail'}, 'authoring-group-files')}>
        <div className="rail-heading"><h2 id="authoring-rail-heading">{a.railHeading}</h2>{railOpen && railButton}</div>
        <div id="authoring-rail-body" className="rail-body">
          <ul className="file-tree rail-groups">
            <li onContextMenu={contextAt({kind: 'files'}, 'authoring-group-files')}>
              {groupRow('files', a.files, {kind: 'files'})}
              <div id="authoring-group-files-items" className="tree-children" hidden={!groups.files}>
                <FileTree drafts={drafts.filter(draft => !draft.missing)} selected={session.selected} reveal={revealRequest}
                  onSelect={path => handlers.select(path, previous())} onMenu={openMenu} menuOpen={openKey}/>
                {missing.length > 0 && <section className="rail-missing" aria-labelledby="authoring-missing-heading">
                  <h3 id="authoring-missing-heading">{a.missingHeading}</h3><p className="field-help">{a.missingHelp}</p>
                  <ul id="authoring-missing" className="file-tree">{missing.map(draft => <li key={draft.path}>
                    <div className={draft.path === session.selected ? 'tree-row current' : 'tree-row'}>
                      <button type="button" className="tree-file" data-path={draft.path} title={draft.path} aria-current={draft.path === session.selected ? 'true' : undefined}
                        onClick={() => handlers.select(draft.path, previous())}><span className="tree-name mono">{draft.path}</span>
                        <span className="tree-meta"><span className="tag">{a.kind(draft.kind)}</span><span className="tag stale">{a.unsaved}</span></span></button>
                    </div></li>)}</ul>
                </section>}
              </div>
            </li>
            <li onContextMenu={contextAt({kind: 'metadataGroup'}, 'authoring-group-metadata')}>
              {groupRow('metadata', a.metadataHeading, {kind: 'metadataGroup'})}
              <ul id="authoring-group-metadata-items" className="file-tree tree-children" hidden={!groups.metadata}>
                {metadata.map(draft => {
                  const target: MenuTarget = {kind: 'metadata', path: draft.path};
                  const current = draft.path === session.selected;
                  return <li key={draft.path}><div className={current ? 'tree-row current' : 'tree-row'}>
                    <button type="button" className="tree-file" data-path={draft.path} data-kind={draft.kind} title={draft.path} aria-current={current ? 'true' : undefined}
                      aria-keyshortcuts="Shift+F10" onClick={() => handlers.select(draft.path, previous())}
                      {...menuEvents((anchor, opener) => openMenu(target, anchor, opener, false))}>
                      <span className="tree-name">{metadataLabel(draft)}</span>
                      {(fileDirty(draft) || draft.diskChanged) && <span className="tree-meta">
                        {fileDirty(draft) && <span className="tag unsaved">{a.unsaved}</span>}
                        {draft.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
                    </button>
                    {menuTrigger(target)}
                  </div></li>;
                })}
              </ul>
            </li>
            <li><div className="tree-row">
              <button id="authoring-duplicate-open" type="button" className="tree-folder tree-action" aria-haspopup="dialog" onClick={() => setDuplicating(true)}>
                <span className="copy-icon" aria-hidden="true"/><span className="tree-name">{a.duplicateOpen}</span></button>
            </div></li>
          </ul>
          <p className="field-help rail-help">{a.filesHelp}</p>
        </div>
      </aside>
      <section id="authoring-main" className="panel repo-main" aria-labelledby="authoring-file-heading">
        <div className="main-bar">
          {!railOpen && railButton}
          {selected
            ? <div className="file-title"><span className="eyebrow">{`${selectedGroup === 'files' ? a.files : a.metadataHeading} · ${a.kind(selected.kind)}`}</span>
              <h2 id="authoring-file-heading" className="file-crumbs mono">{selected.path.split('/').map((part, index, parts) => <Fragment key={index}>
                {index > 0 && <span className="crumb-separator">/</span>}{index === parts.length - 1 ? <strong>{part}</strong> : part}</Fragment>)}</h2></div>
            : <h2 id="authoring-file-heading" className="file-title">{a.noFile}</h2>}
          {selected && (fileDirty(selected) || selected.diskChanged) && <span className="tree-meta">
            {fileDirty(selected) && <span id="authoring-file-dirty" className="tag unsaved">{a.unsaved}</span>}
            {selected.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>}
        </div>
        {selected && <div className="repo-body">{fileView}</div>}
      </section>
    </div>
    {menu && <ContextMenu key={menu.serial} id="authoring-menu" label={menuLabel(menu.target)} anchor={menu.anchor} actions={menuActions(menu.target)}
      heading={'path' in menu.target ? menu.target.kind === 'folder' ? `${menu.target.path}/` : menu.target.path : undefined}
      toggle={menu.toggle} onClose={closeMenu}/>}
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
    <section id="authoring-validation" className="panel validation-panel" aria-labelledby="authoring-validation-heading">
      <div className="panel-heading"><h2 id="authoring-validation-heading">{a.validationHeading}</h2>
        {validation && <span id="authoring-validation-state" className={`tag ${!current ? 'stale' : validation.valid ? 'current' : 'unsaved'}`}>
          {validation.valid ? a.valid(shortRevision(validation.revision)) : a.invalid(shortRevision(validation.revision), validation.diagnostics.length)}</span>}</div>
      <div className="panel-body">
        {validating && <div className="button-row"><span className="muted" role="status">{a.validating}</span>
          <button id="authoring-validate-stop" type="button" className="stop-button" onClick={handlers.stopValidation}>{a.stopValidation}</button></div>}
        {!validation && !validating && <p className="muted">{a.validationNone}</p>}
        {validation && !current && <p className="inline-warning">{a.staleValidation(shortRevision(validation.revision))}</p>}
        {dirty.length > 0 && <p className="field-help">{a.unsavedNotValidated}</p>}
        {validation && validation.diagnostics.length > 0 && <ol id="authoring-diagnostics" className="diagnostic-list">
          {validation.diagnostics.map((item, index) => {
            const location = diagnosticLocation(item);
            const where = location ? a.location(location.path, location.line, location.column) : null;
            return <li key={index}>
              {where && session.drafts.has(location!.path)
                ? <button type="button" className="link diagnostic-location" data-path={location!.path} aria-label={a.goTo(where)} onClick={() => goTo(item)}>{where}</button>
                : <span className="muted">{where ?? a.unlocated}</span>}
              <strong> {item.category}</strong> <span>{item.message}</span></li>;
          })}</ol>}
      </div>
    </section>
  </>;
}

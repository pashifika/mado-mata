import {useEffect, useLayoutEffect, useMemo, useRef, useState} from 'react';
import type {RefObject} from 'react';
import {FaultMessage} from '../components/ResultPanel.tsx';
import Select from '../components/Select.tsx';
import {AUTHORING_RECOVERY, catalogBlock, diagnosticLocation, dirtyDrafts, draftList, fileDirty, findMatch, lineColumn, lineCount, matchSummary, offsetAt, saveBlock, shortRevision, validationCurrent} from '../authoring.ts';
import type {AuthoringSession, EditInput, FileDraft, Snapshot, TextRange} from '../authoring.ts';
import type {CatalogAddKind, CatalogEdit, Fault} from '../types.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// One editor line in CSS pixels; `.code-editor` in style.css uses the same line height for text and gutter.
const LINE_HEIGHT = 18;
const INDENT = '  ';

export interface EditHandlers {
  select: (path: string, previous: TextRange | null) => void;
  edit: (path: string, next: Snapshot, before: TextRange, input: EditInput) => void;
  compositionStart: (path: string, range: TextRange) => void;
  compositionEnd: (path: string) => void;
  range: (path: string, range: TextRange) => void;
  undo: (path: string) => void; redo: (path: string) => void;
  reveal: (path: string, range: TextRange) => void;
  discard: (path: string) => void;
  save: (path: string) => void; saveAll: () => void;
  validate: () => void; stopValidation: () => void;
  refresh: () => void; recover: () => void;
  // Resolves true once the host committed the edit, so the form can be cleared.
  catalog: (edit: CatalogEdit) => Promise<boolean>;
  duplicate: (path: string, packageId: string) => void;
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

function starterText(kind: CatalogAddKind, packageId: string): string {
  if (kind === 'profile') return `${JSON.stringify({package_id: packageId, schema_version: 1, options: {}}, null, 2)}\n`;
  if (kind === 'source_map') return `${JSON.stringify({version: 3, sources: [], names: [], mappings: ''})}\n`;
  return kind === 'asset' ? '{}\n' : '';
}

export default function EditPage({session, label, handlers, locked, lockReason, leaseLost, validationActive}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const a = t.authoring;
  const drafts = draftList(session);
  const dirty = dirtyDrafts(session);
  const selected = session.selected === null ? undefined : session.drafts.get(session.selected);
  const editable = selected !== undefined && selected.text !== null ? selected as FileDraft & {text: string} : undefined;
  const selection = useRef<TextRange>(selected?.range ?? {start: 0, end: 0});
  const searchInput = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [addKind, setAddKind] = useState<CatalogAddKind>('source');
  const [addPath, setAddPath] = useState('');
  const [addId, setAddId] = useState('');
  const [addModule, setAddModule] = useState('');
  const [destination, setDestination] = useState(selected?.path ?? '');
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [duplicatePath, setDuplicatePath] = useState('');
  const [duplicateId, setDuplicateId] = useState('');
  useEffect(() => {
    setDestination(selected?.path ?? '');
    setConfirmRemove(false);
  }, [selected?.path]);

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
  function goTo(item: Fault) {
    const location = diagnosticLocation(item);
    const target = location ? session.drafts.get(location.path) : undefined;
    if (!location || !target || target.text === null) return;
    const offset = offsetAt(target.text, location.line, location.column);
    handlers.reveal(location.path, {start: offset, end: offset});
  }
  function addEdit(): CatalogEdit {
    const path = addPath.trim();
    const text = starterText(addKind, session.packageId);
    if (addKind === 'profile') return {kind: 'add', path, file_kind: 'profile', id: addId.trim(), text};
    if (addKind === 'source_map') return {kind: 'add', path, file_kind: 'source_map', module: addModule.trim(), text};
    if (addKind === 'asset') return {kind: 'add', path, file_kind: 'asset', id: addId.trim(), format: 'json', width: 0, height: 0, text};
    return {kind: 'add', path, file_kind: 'source', text};
  }
  const addReady = addPath.trim() !== '' && ((addKind !== 'profile' && addKind !== 'asset') || addId.trim() !== '') && (addKind !== 'source_map' || addModule.trim() !== '');
  const addReason = catalogBlock(session, {kind: 'add', path: addPath.trim(), file_kind: addKind, text: ''});
  const targetReason = selected ? catalogBlock(session, {kind: 'remove', path: selected.path}) : null;
  const renameReady = selected !== undefined && destination.trim() !== '' && destination.trim() !== selected.path;
  const validation = session.validation;
  const current = validationCurrent(session);
  const statusText = renderMessage(locale, session.notice) || (lockReason ?? '');
  // A publication or a validation that reached disk capture can report an interrupted save that needs recovery.
  const recoveryFault = session.error?.category === AUTHORING_RECOVERY || (current && validation?.diagnostics.some(item => item.category === AUTHORING_RECOVERY) === true);

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
    <div className="edit-grid">
      <section className="panel" aria-labelledby="authoring-files-heading">
        <div className="panel-heading"><h2 id="authoring-files-heading">{a.files}</h2>
          {dirty.length > 0 && <span id="authoring-unsaved-count" className="tag unsaved">{a.unsavedFiles(dirty.length)}</span>}</div>
        <div className="panel-body">
          <p className="field-help">{a.filesHelp}</p>
          <ul id="authoring-tree" className="file-tree">
            {drafts.filter(draft => !draft.missing).map(draft => <li key={draft.path}>
              <button type="button" className="tree-file" data-path={draft.path} aria-current={draft.path === session.selected ? 'true' : undefined}
                onClick={() => handlers.select(draft.path, selection.current)}>
                <span className="tree-path mono">{draft.path}</span>
                <span className="tree-meta"><span className="tag">{a.kind(draft.kind)}</span>
                  {fileDirty(draft) && <span className="tag unsaved">{a.unsaved}</span>}
                  {draft.diskChanged && <span className="tag stale">{a.diskChanged}</span>}</span>
              </button></li>)}
          </ul>
          {drafts.some(draft => draft.missing) && <>
            <h3>{a.missingHeading}</h3><p className="field-help">{a.missingHelp}</p>
            <ul id="authoring-missing" className="file-tree">{drafts.filter(draft => draft.missing).map(draft => <li key={draft.path}>
              <button type="button" className="tree-file" data-path={draft.path} aria-current={draft.path === session.selected ? 'true' : undefined}
                onClick={() => handlers.select(draft.path, selection.current)}><span className="tree-path mono">{draft.path}</span>
                <span className="tree-meta"><span className="tag stale">{a.unsaved}</span></span></button></li>)}</ul></>}
          <dl className="run-identity authoring-identity"><dt>{a.path}</dt><dd id="authoring-package-path" className="mono">{session.packagePath}</dd>
            <dt>{a.revision}</dt><dd id="authoring-revision" className="mono" title={session.revision}>{shortRevision(session.revision)}</dd></dl>
          <details id="authoring-manage" className="authoring-manage"><summary>{a.manage}</summary>
            <h3>{a.addHeading}</h3>
            <div className="field"><label htmlFor="authoring-add-kind">{a.kindLabel}</label>
              <Select id="authoring-add-kind" value={addKind} onChange={value => {if (value === 'source' || value === 'profile' || value === 'asset' || value === 'source_map') setAddKind(value);}}
                options={(['source', 'profile', 'asset', 'source_map'] as const).map(kind => ({value: kind, label: a.kind(kind)}))}/></div>
            <div className="field"><label htmlFor="authoring-add-path">{a.filePath}</label>
              <input id="authoring-add-path" type="text" value={addPath} spellCheck={false} placeholder={a.filePathPlaceholder} onChange={event => setAddPath(event.target.value)}/></div>
            {(addKind === 'profile' || addKind === 'asset') && <div className="field"><label htmlFor="authoring-add-id">{a.id}</label>
              <input id="authoring-add-id" type="text" value={addId} spellCheck={false} onChange={event => setAddId(event.target.value)}/>
              <p className="field-help">{a.idHelp}</p></div>}
            {addKind === 'source_map' && <div className="field"><label htmlFor="authoring-add-module">{a.module}</label>
              <input id="authoring-add-module" type="text" value={addModule} spellCheck={false} onChange={event => setAddModule(event.target.value)}/>
              <p className="field-help">{a.moduleHelp}</p></div>}
            <div className="button-row"><button id="authoring-add-submit" type="button" disabled={publishLocked || !addReady || addReason !== null}
              title={addReason ? a.block(addReason) : undefined}
              onClick={() => void handlers.catalog(addEdit()).then(done => {if (done) {setAddPath(''); setAddId(''); setAddModule('');}})}>{a.add}</button></div>
            <p className="field-help">{a.addHelp}</p>
            {selected && !selected.missing && <>
              <h3>{a.renameHeading(selected.path)}</h3>
              <div className="field"><label htmlFor="authoring-rename-destination">{a.destination}</label>
                <input id="authoring-rename-destination" type="text" value={destination} spellCheck={false} onChange={event => setDestination(event.target.value)}/>
                <p className="field-help">{a.renameHelp}</p></div>
              <div className="button-row">
                <button id="authoring-rename-submit" type="button" disabled={publishLocked || !renameReady || targetReason !== null} title={targetReason ? a.block(targetReason) : undefined}
                  onClick={() => void handlers.catalog({kind: 'rename', path: selected.path, destination: destination.trim()})}>{a.rename}</button>
                {confirmRemove
                  ? <span className="confirm-row" role="alertdialog" aria-labelledby="authoring-remove-text"><span id="authoring-remove-text">{a.confirmRemove(selected.path)}</span>
                    <button id="authoring-remove-confirm" type="button" className="danger-text" disabled={publishLocked || targetReason !== null}
                      onClick={() => {setConfirmRemove(false); void handlers.catalog({kind: 'remove', path: selected.path});}}>{a.removeConfirmed}</button>
                    <button id="authoring-remove-cancel" type="button" autoFocus onClick={() => setConfirmRemove(false)}>{a.cancel}</button></span>
                  : <button id="authoring-remove" type="button" className="danger-text" disabled={publishLocked || targetReason !== null} title={targetReason ? a.block(targetReason) : undefined}
                    onClick={() => setConfirmRemove(true)}>{a.remove}</button>}
              </div></>}
          </details>
          <details id="authoring-duplicate" className="authoring-manage"><summary>{a.duplicateHeading}</summary>
            <p className="field-help">{a.duplicateHelp}</p>
            <div className="field"><label htmlFor="authoring-duplicate-path">{a.destinationDirectory}</label>
              <input id="authoring-duplicate-path" type="text" value={duplicatePath} spellCheck={false} placeholder={a.packagePlaceholder} onChange={event => setDuplicatePath(event.target.value)}/></div>
            <div className="field"><label htmlFor="authoring-duplicate-id">{a.newPackageId}</label>
              <input id="authoring-duplicate-id" type="text" value={duplicateId} spellCheck={false} onChange={event => setDuplicateId(event.target.value)}/></div>
            <div className="button-row"><button id="authoring-duplicate-submit" type="button" disabled={publishLocked || pending !== null || validating || !duplicatePath.trim() || !duplicateId.trim()}
              onClick={() => handlers.duplicate(duplicatePath.trim(), duplicateId.trim())}>{a.duplicate}</button></div>
          </details>
        </div>
      </section>
      <section className="panel editor-panel" aria-labelledby="authoring-file-heading">
        <div className="panel-heading"><div><span className="eyebrow">{selected ? a.kind(selected.kind) : a.files}</span>
          <h2 id="authoring-file-heading" className="mono">{selected?.path ?? a.noFile}</h2></div>
          {selected && fileDirty(selected) && <span id="authoring-file-dirty" className="tag unsaved">{a.unsaved}</span>}</div>
        <div className="panel-body">
          {selected?.diskChanged && <p className="inline-warning">{a.diskChangedHelp}</p>}
          {selected?.missing && <p className="inline-warning">{a.missingHelp}</p>}
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
          {selected && selected.text === null && <p id="authoring-binary" className="muted">{a.binary(selected.bytes)}</p>}
          {!selected && <p className="muted">{a.noFile}</p>}
        </div>
      </section>
    </div>
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
              {where && session.drafts.get(location!.path)?.text != null
                ? <button type="button" className="link diagnostic-location" data-path={location!.path} aria-label={a.goTo(where)} onClick={() => goTo(item)}>{where}</button>
                : <span className="muted">{where ?? a.unlocated}</span>}
              <strong> {item.category}</strong> <span>{item.message}</span></li>;
          })}</ol>}
      </div>
    </section>
  </>;
}

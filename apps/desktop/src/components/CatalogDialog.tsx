import {useRef, useState} from 'react';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import Select from './Select.tsx';
import type {AuthoringSession} from '../authoring.ts';
import type {CatalogAddKind, CatalogEdit} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// A catalog change chosen from a tree menu. Rename and Remove name the row the menu was opened on, never the
// selected file; Add carries the kind and the folder prefix of the row or group it was opened from.
export type CatalogIntent =
  | {kind: 'add'; fileKind: CatalogAddKind; prefix: string}
  | {kind: 'rename'; path: string}
  | {kind: 'remove'; path: string};

const ADD_KINDS = ['source', 'profile', 'asset', 'source_map'] as const;

function starterText(kind: CatalogAddKind, packageId: string): string {
  if (kind === 'profile') return `${JSON.stringify({package_id: packageId, schema_version: 1, options: {}}, null, 2)}\n`;
  if (kind === 'source_map') return `${JSON.stringify({version: 3, sources: [], names: [], mappings: ''})}\n`;
  return kind === 'asset' ? '{}\n' : '';
}

interface Props {
  intent: CatalogIntent | null; session: AuthoringSession;
  // Why this edit cannot be published now (the page's lock, lease and catalogBlock rules), or null.
  reason: (edit: CatalogEdit) => string | null;
  // Resolves true once the host committed the edit; the dialog then closes.
  onSubmit: (edit: CatalogEdit) => Promise<boolean>;
  onClose: () => void;
  // Focus target when the control that opened the dialog is gone (a renamed or removed row).
  returnFocus: () => HTMLElement | null;
}

interface FormProps {
  session: AuthoringSession; busy: boolean; failed: boolean;
  reason: (edit: CatalogEdit) => string | null;
  submit: (edit: CatalogEdit) => void; onClose: () => void;
}

// Add, Rename and Remove as small modal forms. The host's refusal (for example, removing a required entry) keeps the
// form open with the fault; Cancel and Escape change nothing, and neither is available while the edit is in flight.
export default function CatalogDialog({intent, session, reason, onSubmit, onClose, returnFocus}: Props) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [shown, setShown] = useState(intent);
  if (shown !== intent) {
    setShown(intent);
    setFailed(false);
  }

  function submit(edit: CatalogEdit) {
    setBusy(true);
    setFailed(false);
    void onSubmit(edit).then(done => {
      setBusy(false);
      if (done) onClose();
      else setFailed(true);
    });
  }
  const form = {session, busy, failed, reason, submit, onClose};
  const modal = {onCancel: onClose, locked: busy, className: 'catalog-dialog', returnFocus};
  return <>
    <Modal id="authoring-add-dialog" open={intent?.kind === 'add'} labelledBy="authoring-add-heading" initialFocus="#authoring-add-path" {...modal}>
      {intent?.kind === 'add' && <AddForm {...form} kind={intent.fileKind} prefix={intent.prefix}/>}
    </Modal>
    <Modal id="authoring-rename-dialog" open={intent?.kind === 'rename'} labelledBy="authoring-rename-heading" initialFocus="#authoring-rename-destination" {...modal}>
      {intent?.kind === 'rename' && <RenameForm {...form} path={intent.path}/>}
    </Modal>
    <Modal id="authoring-remove-dialog" open={intent?.kind === 'remove'} labelledBy="authoring-remove-heading" initialFocus="#authoring-remove-cancel" {...modal}>
      {intent?.kind === 'remove' && <RemoveForm {...form} path={intent.path}/>}
    </Modal>
  </>;
}

function Footer({id, busy, blocked, disabled, label, danger, onClose}: {id: string; busy: boolean; blocked: string | null; disabled: boolean; label: string; danger?: boolean; onClose: () => void}) {
  const a = messages[useLocale()].ui.authoring;
  return <div className="dialog-footer">
    <span id={`${id}-status`} role="status">{busy ? a.working : blocked ?? ''}</span>
    <button id={`${id}-cancel`} type="button" disabled={busy} onClick={onClose}>{a.cancel}</button>
    <button id={`${id}-submit`} type="submit" className={danger ? 'danger-text' : 'primary'} disabled={busy || disabled || blocked !== null}>{label}</button>
  </div>;
}

function Failure({session, failed}: {session: AuthoringSession; failed: boolean}) {
  const a = messages[useLocale()].ui.authoring;
  return failed && session.error ? <FaultMessage title={a.actionFailed} value={session.error}/> : null;
}

// Selects the text once when the dialog focuses the field, so the prefix or file name is ready to type over.
function useSelectOnce(range: (value: string) => [number, number]) {
  const done = useRef(false);
  return (event: {currentTarget: HTMLInputElement}) => {
    if (done.current) return;
    done.current = true;
    event.currentTarget.setSelectionRange(...range(event.currentTarget.value));
  };
}

function AddForm({session, busy, failed, reason, submit, onClose, kind: initialKind, prefix}: FormProps & {kind: CatalogAddKind; prefix: string}) {
  const a = messages[useLocale()].ui.authoring;
  const [kind, setKind] = useState<CatalogAddKind>(initialKind);
  const [path, setPath] = useState(prefix);
  const [id, setId] = useState('');
  const [module, setModule] = useState('');
  const caretAtEnd = useSelectOnce(value => [value.length, value.length]);
  const target = path.trim();
  const ready = target !== '' && !target.endsWith('/') && ((kind !== 'profile' && kind !== 'asset') || id.trim() !== '') && (kind !== 'source_map' || module.trim() !== '');
  const blocked = reason({kind: 'add', path: target, file_kind: kind, text: ''});
  function edit(): CatalogEdit {
    const text = starterText(kind, session.packageId);
    if (kind === 'profile') return {kind: 'add', path: target, file_kind: 'profile', id: id.trim(), text};
    if (kind === 'source_map') return {kind: 'add', path: target, file_kind: 'source_map', module: module.trim(), text};
    if (kind === 'asset') return {kind: 'add', path: target, file_kind: 'asset', id: id.trim(), format: 'json', width: 0, height: 0, text};
    return {kind: 'add', path: target, file_kind: 'source', text};
  }
  return <form onSubmit={event => {event.preventDefault(); if (!busy && ready && blocked === null) submit(edit());}}>
    <div className="dialog-header"><div><h2 id="authoring-add-heading">{a.addHeading}</h2><p>{a.addHelp}</p></div></div>
    <div className="dialog-body">
      <div className="field"><label htmlFor="authoring-add-kind">{a.kindLabel}</label>
        <Select id="authoring-add-kind" value={kind} disabled={busy} onChange={value => {if (value === 'source' || value === 'profile' || value === 'asset' || value === 'source_map') setKind(value);}}
          options={ADD_KINDS.map(value => ({value, label: a.kind(value)}))}/></div>
      <div className="field"><label htmlFor="authoring-add-path">{a.filePath}</label>
        <input id="authoring-add-path" type="text" value={path} spellCheck={false} autoCapitalize="off" autoCorrect="off" placeholder={a.filePathPlaceholder}
          aria-describedby="authoring-add-folders" readOnly={busy} onFocus={caretAtEnd} onChange={event => setPath(event.target.value)}/>
        <p id="authoring-add-folders" className="field-help">{a.foldersHelp}</p></div>
      {(kind === 'profile' || kind === 'asset') && <div className="field"><label htmlFor="authoring-add-id">{a.id}</label>
        <input id="authoring-add-id" type="text" value={id} spellCheck={false} autoCapitalize="off" autoCorrect="off" readOnly={busy} onChange={event => setId(event.target.value)}/>
        <p className="field-help">{a.idHelp}</p></div>}
      {kind === 'source_map' && <div className="field"><label htmlFor="authoring-add-module">{a.module}</label>
        <input id="authoring-add-module" type="text" value={module} spellCheck={false} autoCapitalize="off" autoCorrect="off" readOnly={busy} onChange={event => setModule(event.target.value)}/>
        <p className="field-help">{a.moduleHelp}</p></div>}
      <Failure session={session} failed={failed}/>
      <Footer id="authoring-add" busy={busy} blocked={blocked} disabled={!ready} label={a.add} onClose={onClose}/>
    </div>
  </form>;
}

// A rename or removal whose row disappeared (a refresh or another change) is refused instead of acting on a new file.
function targetBlock(session: AuthoringSession, path: string, reason: FormProps['reason'], gone: string): string | null {
  const draft = session.drafts.get(path);
  return !draft || draft.missing ? gone : reason({kind: 'remove', path});
}

function RenameForm({session, busy, failed, reason, submit, onClose, path}: FormProps & {path: string}) {
  const a = messages[useLocale()].ui.authoring;
  const [destination, setDestination] = useState(path);
  // The file name without its extension, as in a file browser.
  const selectName = useSelectOnce(value => {
    const start = value.lastIndexOf('/') + 1;
    const dot = value.lastIndexOf('.');
    return [start, dot > start ? dot : value.length];
  });
  const target = destination.trim();
  const blocked = targetBlock(session, path, reason, a.targetGone);
  const ready = target !== '' && target !== path;
  return <form onSubmit={event => {event.preventDefault(); if (!busy && ready && blocked === null) submit({kind: 'rename', path, destination: target});}}>
    <div className="dialog-header"><div><h2 id="authoring-rename-heading">{a.renameHeading(path)}</h2><p>{a.renameHelp}</p></div></div>
    <div className="dialog-body">
      <div className="field"><label htmlFor="authoring-rename-destination">{a.destination}</label>
        <input id="authoring-rename-destination" type="text" value={destination} spellCheck={false} autoCapitalize="off" autoCorrect="off"
          aria-describedby="authoring-rename-folders" readOnly={busy} onFocus={selectName} onChange={event => setDestination(event.target.value)}/>
        <p id="authoring-rename-folders" className="field-help">{a.foldersHelp}</p></div>
      <Failure session={session} failed={failed}/>
      <Footer id="authoring-rename" busy={busy} blocked={blocked} disabled={!ready} label={a.rename} onClose={onClose}/>
    </div>
  </form>;
}

function RemoveForm({session, busy, failed, reason, submit, onClose, path}: FormProps & {path: string}) {
  const a = messages[useLocale()].ui.authoring;
  const blocked = targetBlock(session, path, reason, a.targetGone);
  return <form onSubmit={event => {event.preventDefault(); if (!busy && blocked === null) submit({kind: 'remove', path});}}>
    <div className="dialog-header"><div><h2 id="authoring-remove-heading">{a.removeHeading}</h2></div></div>
    <div className="dialog-body">
      <p id="authoring-remove-text">{a.confirmRemove(path)}</p>
      <Failure session={session} failed={failed}/>
      <Footer id="authoring-remove" busy={busy} blocked={blocked} disabled={false} label={a.removeConfirmed} danger onClose={onClose}/>
    </div>
  </form>;
}

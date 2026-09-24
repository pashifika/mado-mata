import type {ReactNode} from 'react';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {DISPLAY_NAME_LIMIT, INTERNAL_NAME_LIMIT, SAVED_LIMIT, WORKSPACE_LIMIT, displayNameError, internalNameError} from '../workspace.ts';
import type {Fault} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export interface CreateDraft {internalName: string; displayName: string}

interface Props {
  open: boolean; draft: CreateDraft; onDraft: (next: CreateDraft) => void;
  onCancel: () => void; onCreate: () => void;
  creating: boolean; error: Fault | null; openCount: number; savedCount: number;
  // Reason another command keeps Create unavailable; editing and Cancel stay possible.
  busyReason: string | null;
  strip: ReactNode;
}

// Two names, nothing else. The host refuses invalid or colliding names; the local checks only mirror its bounds so
// the operator sees them before a round trip.
export default function CreateWorkspaceDialog({open, draft, onDraft, onCancel, onCreate, creating, error, openCount, savedCount, busyReason, strip}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const internalError = internalNameError(draft.internalName);
  const displayError = displayNameError(draft.displayName);
  const limit = openCount >= WORKSPACE_LIMIT ? t.create.openLimit(WORKSPACE_LIMIT) : savedCount >= SAVED_LIMIT ? t.create.savedLimit(SAVED_LIMIT) : null;
  // Errors show only once the operator typed something; an empty field is not yet a mistake.
  const shownInternal = draft.internalName !== '' ? internalError : null;
  const shownDisplay = draft.displayName !== '' ? displayError : null;
  const blocked = creating || internalError !== null || displayError !== null || limit !== null || busyReason !== null;
  return <Modal id="create-workspace" open={open} onCancel={onCancel} labelledBy="create-heading" locked={creating} initialFocus="#internal-name">
    <div className="dialog-header"><div><h2 id="create-heading">{t.create.heading}</h2><p>{t.create.intro}</p></div>
      <button type="button" className="icon" aria-label={t.create.close} disabled={creating} onClick={onCancel}>×</button></div>
    {strip}
    <form className="dialog-body" onSubmit={event => {event.preventDefault(); if (!blocked) onCreate();}}>
      <div className="field"><label htmlFor="internal-name">{t.create.internal}</label>
        <input id="internal-name" type="text" value={draft.internalName} disabled={creating} spellCheck={false} autoComplete="off"
          aria-invalid={shownInternal !== null} aria-describedby="internal-name-help" onChange={event => onDraft({...draft, internalName: event.target.value})}/>
        {shownInternal !== null && <p className="field-error">{t.create.errors(shownInternal)}</p>}
        <p id="internal-name-help" className="field-help">{t.create.internalHelp} <span className="mono">{t.create.count(draft.internalName.length, INTERNAL_NAME_LIMIT)}</span></p></div>
      <div className="field"><label htmlFor="display-name">{t.create.display}</label>
        <input id="display-name" type="text" value={draft.displayName} disabled={creating} spellCheck={false} autoComplete="off"
          aria-invalid={shownDisplay !== null} aria-describedby="display-name-help" onChange={event => onDraft({...draft, displayName: event.target.value})}/>
        {shownDisplay !== null && <p className="field-error">{t.create.errors(shownDisplay)}</p>}
        <p id="display-name-help" className="field-help">{t.create.displayHelp} <span className="mono">{t.create.count(Array.from(draft.displayName).length, DISPLAY_NAME_LIMIT)}</span></p></div>
      {error && <><FaultMessage title={t.create.failed} value={error}/><p className="muted">{t.create.failedHelp}</p></>}
      <div className="dialog-footer">
        <span role="status">{creating ? t.create.creating : limit ?? busyReason ?? ''}</span>
        <button type="button" id="cancel-create" onClick={onCancel} disabled={creating}>{t.common.cancel}</button>
        <button type="submit" id="submit-create" className="primary" disabled={blocked}>{t.create.create}</button>
      </div>
    </form>
  </Modal>;
}

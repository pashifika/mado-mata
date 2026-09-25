import Modal from './Modal.tsx';
import type {FileDraft} from '../authoring.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export type DirtyIntent = 'exit' | 'duplicate' | 'close' | 'closeTab';

interface Props {
  intent: DirtyIntent | null; drafts: FileDraft[]; busy: boolean;
  // Why Save cannot publish at all (for example, the host no longer reports this Edit session).
  saveBlock: string | null;
  onSave: () => void; onDiscard: () => void; onCancel: () => void;
}

// Save/Discard/Cancel before an action that ends or replaces the Edit session. Cancel (including Escape) leaves the
// lease, every draft and the selected file untouched; the dialog cannot be dismissed while a choice is being carried out.
export default function DirtyChoiceDialog({intent, drafts, busy, saveBlock, onSave, onDiscard, onCancel}: Props) {
  const locale = useLocale();
  const a = messages[locale].ui.authoring;
  const unsavable = drafts.some(draft => draft.missing);
  return <Modal id="authoring-dirty-dialog" open={intent !== null} onCancel={onCancel} labelledBy="authoring-dirty-heading" locked={busy} initialFocus="#authoring-dirty-cancel">
    <div className="dialog-header"><div><h2 id="authoring-dirty-heading">{intent ? a.dirtyHeading(intent) : ''}</h2><p>{a.dirtyHelp}</p></div></div>
    <div className="dialog-body">
      <ul id="authoring-dirty-files" className="saved-list">{drafts.map(draft => <li key={draft.path} className="saved-row">
        <span className="saved-text"><strong className="mono">{draft.path}</strong><span className="mono">{a.kind(draft.kind)}{draft.missing ? ` · ${a.missingHeading}` : ''}</span></span></li>)}</ul>
      {unsavable && <p className="inline-warning">{a.dirtyUnsavable}</p>}
      {saveBlock && <p className="inline-warning">{saveBlock}</p>}
      <div className="dialog-footer">
        <span role="status">{busy ? a.working : a.unsavedFiles(drafts.length)}</span>
        <button id="authoring-dirty-cancel" type="button" disabled={busy} onClick={onCancel}>{a.dirtyCancel}</button>
        <button id="authoring-dirty-discard" type="button" className="danger-text" disabled={busy} onClick={onDiscard}>{a.dirtyDiscard}</button>
        <button id="authoring-dirty-save" type="button" className="primary" disabled={busy || unsavable || saveBlock !== null} onClick={onSave}>{a.dirtySave}</button>
      </div>
    </div>
  </Modal>;
}

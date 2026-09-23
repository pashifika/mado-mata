import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import EnvironmentPanel from './EnvironmentPanel.tsx';
import type {CheckTarget, LastCheck} from './EnvironmentPanel.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {TIMEOUT_SECONDS, VISIBLE_COUNTS} from './state.ts';
import type {EnvironmentDraft, SettingsDraft} from './state.ts';
import type {EditableSettings, Fault, Settings} from './types.ts';

type Category = 'notifications' | 'environment' | 'logs';
const CATEGORIES: {id: Category; label: string}[] = [
  {id: 'notifications', label: 'Notifications'}, {id: 'environment', label: 'OCR environment'}, {id: 'logs', label: 'Logs'},
];

interface Props {
  open: boolean; onCancel: () => void;
  settings: Settings | null; draft: SettingsDraft; onDraft: (next: SettingsDraft) => void;
  parsed: {settings: EditableSettings | null; errors: Record<string, string>};
  dirty: boolean; saving: boolean; saveError: Fault | null; saveNotice: string; onSave: () => void;
  envDirty: boolean; active: boolean; target: CheckTarget; onCheck: () => void; checkError: Fault | null;
  lastCheck: LastCheck | null; stale: string[]; originLabel: (workspaceId: string | null) => string;
  retained: number; evicted: number;
  strip: ReactNode;
}

export default function SettingsDialog(props: Props) {
  const {open, onCancel, settings, draft, onDraft, parsed, dirty, saving, saveError, saveNotice, onSave, envDirty, active, target, onCheck, checkError, lastCheck, stale, originLabel, retained, evicted, strip} = props;
  const dialog = useRef<HTMLDialogElement>(null);
  const opener = useRef<Element | null>(null);
  const [category, setCategory] = useState<Category>('notifications');
  // Native modal semantics: the rest of the document is inert, Escape raises cancel, and focus returns to the opener.
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) {
      opener.current = document.activeElement;
      element.showModal();
      element.querySelector<HTMLElement>('.settings-nav button[aria-selected="true"]')?.focus();
    } else if (!open && element.open) {
      element.close();
      if (opener.current instanceof HTMLElement) opener.current.focus();
    }
  }, [open]);
  const errors = parsed.errors;
  return <dialog id="app-settings" ref={dialog} className="settings-dialog" aria-labelledby="settings-heading"
    onCancel={event => {event.preventDefault(); if (!saving) onCancel();}}>
    {open && <>
      <div className="dialog-header"><div><h2 id="settings-heading">App settings</h2><p>Shared across all workspaces. Your workspace stays where it is. Opening this dialog performs no native operation.</p></div>
        <button type="button" className="icon" aria-label="Close settings without saving" disabled={saving} onClick={onCancel}>×</button></div>
      {strip}
      <div className="dialog-layout">
        <nav className="settings-nav" role="tablist" aria-label="Settings categories">
          {CATEGORIES.map(item => <button key={item.id} id={`settings-tab-${item.id}`} type="button" role="tab" aria-selected={category === item.id} aria-controls="settings-content" onClick={() => setCategory(item.id)}>{item.label}</button>)}
        </nav>
        <div id="settings-content" className="settings-content" role="tabpanel">
          {category === 'notifications' && <section aria-labelledby="notifications-heading">
            <h3 id="notifications-heading">Quiet, but not invisible.</h3>
            <p className="muted">Short-lived cards at the bottom right. Blocking failures and cleanup outcomes remain on the Run page regardless of cards.</p>
            <div className="field"><label htmlFor="visible-count">Visible notifications</label>
              <select id="visible-count" value={draft.notifications.visible_count} aria-invalid={Boolean(errors.visibleCount)}
                onChange={event => onDraft({...draft, notifications: {...draft.notifications, visible_count: Number(event.target.value)}})}>
                {VISIBLE_COUNTS.map(count => <option key={count} value={count}>{count} {count === 1 ? 'card' : 'cards'}</option>)}
              </select>
              {errors.visibleCount && <p className="field-error">{errors.visibleCount}</p>}
              <p className="field-help">Older cards leave the stack immediately; every event stays in the relevant log.</p></div>
            <div className="field"><label htmlFor="timeout-seconds">Auto-dismiss after</label>
              <select id="timeout-seconds" value={draft.notifications.timeout_seconds} aria-invalid={Boolean(errors.timeoutSeconds)}
                onChange={event => onDraft({...draft, notifications: {...draft.notifications, timeout_seconds: Number(event.target.value)}})}>
                {TIMEOUT_SECONDS.map(seconds => <option key={seconds} value={seconds}>{seconds} seconds</option>)}
              </select>
              {errors.timeoutSeconds && <p className="field-error">{errors.timeoutSeconds}</p>}
              <p className="field-help">The timer pauses while you hover or focus a card. Cards already shown keep the duration they were created with.</p></div>
            <div className="switch-row"><label htmlFor="show-success">Show success notifications</label>
              <input id="show-success" type="checkbox" checked={draft.notifications.show_success}
                onChange={event => onDraft({...draft, notifications: {...draft.notifications, show_success: event.target.checked}})}/></div>
            <p className="field-help">Warnings and errors are never suppressed by this switch.</p>
          </section>}
          {category === 'logs' && <section aria-labelledby="logs-settings-heading">
            <h3 id="logs-settings-heading">Retained GUI events</h3>
            <p className="muted">One bounded buffer shared by every workspace and application log page. File logs and delivery queues are unaffected.</p>
            <div className="field"><label htmlFor="gui-log-limit">GUI log item limit</label>
              <input id="gui-log-limit" type="text" inputMode="numeric" value={draft.logLimit} aria-invalid={Boolean(errors.logLimit)}
                onChange={event => onDraft({...draft, logLimit: event.target.value})}/>
              {errors.logLimit && <p className="field-error">{errors.logLimit}</p>}
              <p className="field-help">1–10000 · default 1000 · saved limit {settings?.gui_log_limit ?? 'not loaded'}. Lowering it trims the shared history immediately after Save.</p></div>
            <dl className="fixed-facts"><dt>Retained now</dt><dd>{retained}</dd><dt>Display evicted</dt><dd>{evicted}</dd></dl>
          </section>}
          {category === 'environment' && <>
            {checkError && <FaultMessage title="Check was not started" value={checkError}/>}
            <EnvironmentPanel draft={draft.environment} errors={errors} onDraft={(next: EnvironmentDraft) => onDraft({...draft, environment: next})}
              saved={settings?.ocr_environment ?? null} loaded={settings !== null} dirty={envDirty} locked={saving} active={active}
              target={target} onCheck={onCheck} lastCheck={lastCheck} stale={stale} originLabel={originLabel}/>
          </>}
          {saveError && <FaultMessage title="Settings were not saved · draft kept" value={saveError}/>}
        </div>
      </div>
      <div className="dialog-footer">
        <span role="status">{saving ? 'Saving…' : saveError ? 'Previous stored settings are unchanged. Correct the draft and save again.' : dirty ? 'Unsaved changes · Cancel or Escape discards them' : saveNotice || 'No unsaved changes'}</span>
        <button type="button" id="cancel-settings" onClick={onCancel} disabled={saving}>{dirty ? 'Cancel' : 'Close'}</button>
        <button type="button" id="save-settings" className="primary" disabled={saving || !dirty || parsed.settings === null} onClick={onSave}>Save changes</button>
      </div>
    </>}
  </dialog>;
}

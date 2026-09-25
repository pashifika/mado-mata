import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import EnvironmentPanel from './EnvironmentPanel.tsx';
import type {CheckTarget, LastCheck} from './EnvironmentPanel.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import Select from '../components/Select.tsx';
import type {SnapshotOutcome} from '../bootstrap.ts';
import {TIMEOUT_SECONDS, VISIBLE_COUNTS} from '../state.ts';
import type {EnvironmentDraft, SettingsDraft} from '../state.ts';
import type {EditableSettings, Fault, Settings} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

type Category = 'display' | 'notifications' | 'packages' | 'environment' | 'logs' | 'backups';
const CATEGORIES: Category[] = ['display', 'notifications', 'packages', 'environment', 'logs', 'backups'];
// state.ts stays the validation authority; this only maps its error keys to the category whose fields show them.
const ERROR_CATEGORY: Record<string, Category> = {
  locale: 'display',
  visibleCount: 'notifications', timeoutSeconds: 'notifications', showSuccess: 'notifications',
  logLimit: 'logs',
  packagesRoot: 'packages',
  profile: 'environment', model_root: 'environment', runtime_path: 'environment', library_paths: 'environment',
};

interface Props {
  open: boolean; onCancel: () => void;
  settings: Settings | null; draft: SettingsDraft; onDraft: (next: SettingsDraft) => void;
  defaultPackagesRoot: string;
  parsed: {settings: EditableSettings | null; errors: Record<string, string>};
  dirty: boolean; saving: boolean; saveError: Fault | null; saveNotice: string; onSave: () => void;
  // Label of the package/workspace host command that keeps Save and Check unavailable; distinct from saving so
  // Cancel, Escape and editing stay available while another command is in flight.
  busyReason: string | null;
  envDirty: boolean; active: boolean; pickerBusy: boolean; target: CheckTarget; onCheck: () => void; checkError: Fault | null;
  // Set while any Tab owns Edit: OCR Check is refused, settings Save is not.
  authoringReason: string | null;
  lastCheck: LastCheck | null; stale: string[]; originLabel: (workspaceId: string | null) => string;
  retained: number; evicted: number;
  // Back up now is separate from Save: it uses the saved destination and its outcome outlives the dialog.
  onSnapshot: () => void; snapshotPending: boolean; snapshotOutcome: SnapshotOutcome | null;
  strip: ReactNode;
}

export default function SettingsDialog(props: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const {open, onCancel, settings, draft, onDraft, parsed, dirty, saving, saveError, saveNotice, onSave, busyReason, envDirty, active, pickerBusy, target, onCheck, checkError, authoringReason, lastCheck, stale, originLabel, retained, evicted, onSnapshot, snapshotPending, snapshotOutcome, strip} = props;
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
  const invalidCount: Record<Category, number> = {display: 0, notifications: 0, packages: 0, environment: 0, logs: 0, backups: 0};
  for (const key of Object.keys(errors)) {
    const owner = ERROR_CATEGORY[key];
    if (owner) invalidCount[owner] += 1;
  }
  const invalid = Object.keys(errors).length;
  const invalidLabels = CATEGORIES.filter(id => invalidCount[id] > 0).map(id => t.common[id]).join(t.settings.categorySeparator);
  const backupDirty = draft.backupDirectory.trim() !== (settings?.backup_directory ?? '');
  // Errors in an unselected category and a pending host command both block Save; the footer names which.
  const status = saving ? t.settings.saving
    : invalid > 0 ? t.settings.invalid(invalid, invalidLabels)
    : dirty && busyReason ? t.settings.wait(busyReason)
    : saveError ? t.settings.saveFailed
    : dirty ? t.settings.unsaved : saveNotice || t.settings.unchanged;
  return <dialog id="app-settings" ref={dialog} className="settings-dialog" aria-labelledby="settings-heading"
    onCancel={event => {event.preventDefault(); if (!saving) onCancel();}}>
    {open && <>
      <div className="dialog-header"><div><h2 id="settings-heading">{t.settings.heading}</h2><p>{t.settings.introduction}</p></div>
        <button type="button" className="icon" aria-label={t.settings.close} disabled={saving} onClick={onCancel}>×</button></div>
      {strip}
      <div className="dialog-layout">
        <nav className="settings-nav" role="tablist" aria-label={t.settings.categories}>
          {CATEGORIES.map(id => <button key={id} id={`settings-tab-${id}`} type="button" role="tab" aria-selected={category === id} aria-controls="settings-content" onClick={() => setCategory(id)}>
            {t.common[id]}{invalidCount[id] > 0 && <span className="field-error"> · {t.settings.invalidCount(invalidCount[id])}</span>}</button>)}
        </nav>
        <div id="settings-content" className="settings-content" role="tabpanel">
          {category === 'display' && <section aria-labelledby="display-heading">
            <h3 id="display-heading">{t.common.display}</h3>
            <div className="field"><label htmlFor="app-locale">{t.common.language}</label>
              <Select id="app-locale" value={draft.locale} aria-invalid={Boolean(errors.locale)}
                onChange={value => {if (value === 'en' || value === 'ja') onDraft({...draft, locale: value});}}
                options={[{value: 'en', label: t.settings.languageNames.en}, {value: 'ja', label: t.settings.languageNames.ja}]}/>
              {errors.locale && <p className="field-error">{errors.locale}</p>}
              <p className="field-help">{t.settings.languageHelp}</p></div>
          </section>}
          {category === 'notifications' && <section aria-labelledby="notifications-heading">
            <h3 id="notifications-heading">{t.settings.notificationsHeading}</h3>
            <p className="muted">{t.settings.notificationsHelp}</p>
            <div className="field"><label htmlFor="visible-count">{t.settings.visible}</label>
              <Select id="visible-count" value={String(draft.notifications.visible_count)} aria-invalid={Boolean(errors.visibleCount)}
                onChange={value => onDraft({...draft, notifications: {...draft.notifications, visible_count: Number(value)}})}
                options={VISIBLE_COUNTS.map(count => ({value: String(count), label: t.settings.cards(count)}))}/>
              {errors.visibleCount && <p className="field-error">{errors.visibleCount}</p>}
              <p className="field-help">{t.settings.visibleHelp}</p></div>
            <div className="field"><label htmlFor="timeout-seconds">{t.settings.timeout}</label>
              <Select id="timeout-seconds" value={String(draft.notifications.timeout_seconds)} aria-invalid={Boolean(errors.timeoutSeconds)}
                onChange={value => onDraft({...draft, notifications: {...draft.notifications, timeout_seconds: Number(value)}})}
                options={TIMEOUT_SECONDS.map(seconds => ({value: String(seconds), label: t.settings.seconds(seconds)}))}/>
              {errors.timeoutSeconds && <p className="field-error">{errors.timeoutSeconds}</p>}
              <p className="field-help">{t.settings.timeoutHelp}</p></div>
            <div className="switch-row"><label htmlFor="show-success">{t.settings.success}</label>
              <input id="show-success" type="checkbox" checked={draft.notifications.show_success}
                onChange={event => onDraft({...draft, notifications: {...draft.notifications, show_success: event.target.checked}})}/></div>
            <p className="field-help">{t.settings.successHelp}</p>
          </section>}
          {category === 'packages' && <section aria-labelledby="packages-heading">
            <h3 id="packages-heading">{t.settings.packagesHeading}</h3>
            <p className="muted">{t.settings.packagesHelp}</p>
            <div className="field"><label htmlFor="packages-root">{t.settings.packagesRoot}</label>
              <input id="packages-root" type="text" value={draft.packagesRoot} spellCheck={false} aria-invalid={Boolean(errors.packagesRoot)}
                placeholder={props.defaultPackagesRoot} onChange={event => onDraft({...draft, packagesRoot: event.target.value})}/>
              {errors.packagesRoot && <p className="field-error">{errors.packagesRoot}</p>}
              <p className="field-help">{t.settings.packagesRootHelp}</p></div>
            <button id="packages-root-default" type="button" disabled={saving || !draft.packagesRoot} onClick={() => onDraft({...draft, packagesRoot: ''})}>{t.settings.packagesDefault}</button>
            <dl className="fixed-facts"><dt>{t.settings.packagesDefaultRoot}</dt><dd className="mono">{props.defaultPackagesRoot}</dd>
              <dt>{t.settings.packagesSavedRoot}</dt><dd className="mono">{settings?.packages_root ?? props.defaultPackagesRoot}</dd></dl>
          </section>}
          {category === 'logs' && <section aria-labelledby="logs-settings-heading">
            <h3 id="logs-settings-heading">{t.settings.logsHeading}</h3>
            <p className="muted">{t.settings.logsHelp}</p>
            <div className="field"><label htmlFor="gui-log-limit">{t.settings.logLimit}</label>
              <input id="gui-log-limit" type="text" inputMode="numeric" value={draft.logLimit} aria-invalid={Boolean(errors.logLimit)}
                onChange={event => onDraft({...draft, logLimit: event.target.value})}/>
              {errors.logLimit && <p className="field-error">{errors.logLimit}</p>}
              <p className="field-help">{t.settings.logLimitHelp(settings?.gui_log_limit ?? null)}</p></div>
            <dl className="fixed-facts"><dt>{t.settings.retained}</dt><dd>{retained}</dd><dt>{t.settings.evicted}</dt><dd>{evicted}</dd></dl>
          </section>}
          {category === 'backups' && <section aria-labelledby="backups-heading">
            <h3 id="backups-heading">{t.settings.backupsHeading}</h3>
            <p className="muted">{t.settings.backupsHelp}</p>
            <div className="field"><label htmlFor="backup-directory">{t.settings.backupDirectory}</label>
              <input id="backup-directory" type="text" value={draft.backupDirectory} spellCheck={false} placeholder={t.settings.backupDirectoryPlaceholder}
                onChange={event => onDraft({...draft, backupDirectory: event.target.value})}/>
              <p className="field-help">{t.settings.backupDirectoryHelp}</p></div>
            <div className="button-row">
              <button id="backup-now" type="button" disabled={settings === null || snapshotPending || saving} onClick={onSnapshot}>{t.settings.backupNow}</button>
              <span className="muted">{t.settings.backupNowHelp}</span>
            </div>
            {backupDirty && <p className="inline-warning">{t.settings.backupUnsaved}</p>}
            {snapshotPending && <p className="muted" role="status">{t.settings.backupPending}</p>}
            {!snapshotPending && snapshotOutcome?.kind === 'receipt' && <div className="receipt" role="status"><strong>{t.settings.backupWritten}</strong>
              <dl className="run-identity"><dt>{t.bootstrap.receiptPath}</dt><dd className="mono">{snapshotOutcome.receipt.path}</dd>
                <dt>{t.bootstrap.receiptGeneration}</dt><dd><code>{snapshotOutcome.receipt.generation}</code></dd>
                <dt>{t.bootstrap.receiptFiles}</dt><dd>{snapshotOutcome.receipt.files}</dd><dt>{t.bootstrap.receiptBytes}</dt><dd>{snapshotOutcome.receipt.bytes}</dd></dl></div>}
            {!snapshotPending && snapshotOutcome?.kind === 'fault' && <FaultMessage title={t.settings.backupFailed} value={snapshotOutcome.fault}/>}
          </section>}
          {category === 'environment' && <>
            {checkError && <FaultMessage title={t.settings.checkFailed} value={checkError}/>}
            <EnvironmentPanel draft={draft.environment} errors={errors} onDraft={(next: EnvironmentDraft) => onDraft({...draft, environment: next})}
              saved={settings?.ocr_environment ?? null} loaded={settings !== null} dirty={envDirty} locked={saving} active={active} pickerBusy={pickerBusy} busyReason={busyReason} authoringReason={authoringReason}
              target={target} onCheck={onCheck} lastCheck={lastCheck} stale={stale} originLabel={originLabel}/>
          </>}
          {saveError && <FaultMessage title={t.settings.saveError} value={saveError}/>}
        </div>
      </div>
      <div className="dialog-footer">
        <span role="status">{status}</span>
        <button type="button" id="cancel-settings" onClick={onCancel} disabled={saving}>{dirty ? t.common.cancel : t.common.close}</button>
        <button type="button" id="save-settings" className="primary" disabled={settings === null || saving || !dirty || parsed.settings === null || busyReason !== null} onClick={onSave}>{t.common.save}</button>
      </div>
    </>}
  </dialog>;
}

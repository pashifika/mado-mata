import type {ReactNode} from 'react';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {SAVED_LIMIT, WORKSPACE_LIMIT} from '../workspace.ts';
import type {Fault, TabRecord} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  open: boolean; onCancel: () => void;
  // Host listing of closed saved Tabs and the malformed records it could not load; null until the first read settles.
  closed: TabRecord[] | null; faults: Fault[]; listError: Fault | null;
  openCount: number; savedCount: number;
  onRefresh: () => void; onReopen: (internalName: string) => void;
  reopening: string | null; reopenError: Fault | null;
  busyReason: string | null;
  strip: ReactNode;
}

// Closed Tabs are saved storage, not deleted sessions. Reopen asks the host for a fresh session; drafts never return.
export default function SavedWorkspacesDialog({open, onCancel, closed, faults, listError, openCount, savedCount, onRefresh, onReopen, reopening, reopenError, busyReason, strip}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const atLimit = openCount >= WORKSPACE_LIMIT;
  const blocked = reopening !== null || busyReason !== null || atLimit;
  return <Modal id="saved-workspaces" open={open} onCancel={onCancel} labelledBy="saved-heading" locked={reopening !== null} initialFocus="#refresh-saved">
    <div className="dialog-header"><div><h2 id="saved-heading">{t.reopen.heading}</h2><p>{t.reopen.intro}</p></div>
      <button type="button" className="icon" aria-label={t.reopen.close} disabled={reopening !== null} onClick={onCancel}>×</button></div>
    {strip}
    <div className="dialog-body">
      <div className="button-row"><button id="refresh-saved" type="button" onClick={onRefresh} disabled={reopening !== null}>{t.reopen.refresh}</button>
        <span className="muted">{t.reopen.saved(savedCount, SAVED_LIMIT)}</span></div>
      {listError && <FaultMessage title={t.bootstrap.catalogFaults} value={listError}/>}
      {reopenError && <><FaultMessage title={t.reopen.failed} value={reopenError}/><p className="muted">{t.reopen.failedHelp}</p></>}
      {closed !== null && closed.length === 0 && <p className="muted">{t.reopen.empty}</p>}
      {closed !== null && closed.length > 0 && <ul className="saved-list" aria-label={t.reopen.heading}>
        {closed.map(tab => {
          const selected = tab.packages.find(item => item.package_id === tab.selected_package_id) ?? null;
          const source = selected === null ? t.reopen.noPackage
            : selected.source.kind === 'directory' ? t.reopen.directory(selected.package_id) : t.reopen.archive(selected.package_id);
          return <li key={tab.internal_name} className="saved-row">
            <div className="saved-text"><strong>{tab.display_name}</strong><span className="mono">{tab.internal_name}</span>
              <span className="muted">{source}{tab.packages.length > 1 ? ` · ${t.reopen.references(tab.packages.length)}` : ''}</span></div>
            <button type="button" className="primary" disabled={blocked} aria-label={t.reopen.reopenLabel(`${tab.display_name} · ${tab.internal_name}`)}
              title={atLimit ? t.reopen.limit(WORKSPACE_LIMIT) : busyReason ?? undefined} onClick={() => onReopen(tab.internal_name)}>
              {reopening === tab.internal_name ? t.reopen.reopening : t.reopen.reopen}</button>
          </li>;
        })}
      </ul>}
      {faults.length > 0 && <section className="catalog-faults"><h3>{t.bootstrap.catalogFaults}</h3><p className="muted">{t.bootstrap.catalogFaultsHelp}</p>
        {faults.map((fault, index) => <FaultMessage key={index} title={t.bootstrap.catalogFaults} value={fault}/>)}</section>}
      <div className="dialog-footer">
        <span role="status">{atLimit ? t.reopen.limit(WORKSPACE_LIMIT) : busyReason ?? ''}</span>
        <button type="button" id="close-saved" onClick={onCancel} disabled={reopening !== null}>{t.common.close}</button>
      </div>
    </div>
  </Modal>;
}

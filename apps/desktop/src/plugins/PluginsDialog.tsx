import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent, ReactNode} from 'react';
import Modal from '../components/Modal.tsx';
import {AuthoringFooterStatus} from '../components/StatusSurface.tsx';
import type {AuthoringStatusProps} from '../components/StatusSurface.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {AGENT_DETAILS_CONTROL, AGENT_INTEGRATION, AgentIntegrationDetail, AgentIntegrationEntry} from './AgentIntegration.tsx';
import type {PluginAction} from './plugin-management.ts';
import type {PluginManagementControls} from './usePluginManagement.ts';
import './plugins.css';

interface Props {
  open: boolean; onClose: () => void; plugins: PluginManagementControls;
  // Why management cannot be admitted now (Setup not complete, application closing); null when it can.
  unavailable: string | null;
  strip: ReactNode; authoring: AuthoringStatusProps | null;
}

const TABS = ['system', 'external'] as const;
type Tab = typeof TABS[number];
// Local navigation only: it never reads, changes or forgets plugin management state.
type Page = {kind: 'list'; tab: Tab} | {kind: 'detail'; plugin: typeof AGENT_INTEGRATION};
const SYSTEM_LIST: Page = {kind: 'list', tab: 'system'};

export default function PluginsDialog({open, onClose, plugins: {state, session}, unavailable, strip, authoring}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const p = ui.plugins;
  const [page, setPage] = useState<Page>(SYSTEM_LIST);
  const [shown, setShown] = useState(open);
  const [focusRequest, setFocusRequest] = useState<string | null>(null);
  // Every opening starts at the System list; pending work and outcomes stay attributable there and in Details.
  if (shown !== open) {
    setShown(open);
    if (open) {
      setPage(SYSTEM_LIST);
      setFocusRequest(null);
    }
  }
  const wasApplying = useRef(state.applying !== null);
  // The read control the operator used last; focus returns to it after its read settles.
  const readControl = useRef('#plugins-refresh');
  // Move focus to a requested control, and keep it inside the dialog when the focused control unmounts. Focus that
  // is already inside the dialog, such as on the list after Back, is never taken by a read or change settling.
  useEffect(() => {
    const settled = wasApplying.current && state.applying === null;
    wasApplying.current = state.applying !== null;
    const dialog = document.getElementById('plugins');
    if (!open || !dialog?.hasAttribute('open')) return;
    if (focusRequest !== null) setFocusRequest(null);
    const requested = focusRequest === null ? null : dialog.querySelector<HTMLElement>(focusRequest);
    if (requested && !requested.matches(':disabled')) {
      requested.focus();
      requested.closest('.plugin-confirmation')?.scrollIntoView({block: 'nearest'});
      return;
    }
    const active = document.activeElement;
    if (active !== null && active !== document.body && active !== dialog && dialog.contains(active)) return;
    // On Details: progress, then a settled read-back result, then the last read control take the lost focus.
    const fallbacks = page.kind === 'detail'
      ? state.inspecting ? null : ['#plugin-pending', settled ? '#plugin-result' : null, readControl.current, '#plugins-refresh', '#plugins-back']
      : [`#${AGENT_DETAILS_CONTROL}`, `#plugins-tab-${page.tab}`];
    if (fallbacks === null) return;
    for (const selector of [...fallbacks, '#close-plugins']) {
      const element = selector === null ? null : dialog.querySelector<HTMLElement>(selector);
      if (element && !element.matches(':disabled')) {
        element.focus();
        return;
      }
    }
  });

  const admitted = unavailable === null;
  const applying = state.applying;
  const status = applying ? p.running[applying.action] : state.inspecting ? p.inspecting : '';
  const tab = page.kind === 'list' ? page.tab : null;

  // Leaving a page withdraws its unconfirmed review; admitted reads and changes continue and report back.
  function navigate(next: Page, focus: string) {
    session.dismiss();
    setPage(next);
    setFocusRequest(focus);
  }
  function tabKey(event: KeyboardEvent<HTMLDivElement>) {
    if (tab === null) return;
    const index = TABS.indexOf(tab);
    const next = event.key === 'ArrowRight' ? TABS[(index + 1) % TABS.length]
      : event.key === 'ArrowLeft' ? TABS[(index + TABS.length - 1) % TABS.length]
      : event.key === 'Home' ? TABS[0] : event.key === 'End' ? TABS[TABS.length - 1] : null;
    if (next === null) return;
    event.preventDefault();
    navigate({kind: 'list', tab: next}, `#plugins-tab-${next}`);
  }
  function read(control: '#plugins-refresh' | '#plugin-check') {
    readControl.current = control;
    void (control === '#plugins-refresh' ? session.refresh() : session.check());
  }
  function review(action: PluginAction) {
    if (!admitted) return;
    session.review(action);
    setFocusRequest('#plugin-confirm-cancel');
  }
  function cancelReview() {
    const action = state.confirmation?.action;
    session.dismiss();
    if (action) setFocusRequest(`#plugin-action-${action}`);
  }
  function confirm() {
    if (!admitted) return;
    void session.confirm();
    setFocusRequest('#plugin-pending');
  }

  return <Modal id="plugins" className="plugins-dialog" open={open} labelledBy="plugins-heading"
    onCancel={() => {if (state.confirmation) cancelReview(); else onClose();}} initialFocus="#plugins-tab-system">
    <div className="dialog-header"><h2 id="plugins-heading">{p.heading}</h2>
      <button type="button" className="icon" aria-label={p.close} title={p.close} onClick={onClose}>×</button></div>
    {strip}
    <div className="plugins-nav">
      {tab !== null
        ? <div className="plugins-tabs" role="tablist" aria-label={p.sources} onKeyDown={tabKey}>
          {TABS.map(id => <button key={id} id={`plugins-tab-${id}`} type="button" role="tab" aria-selected={tab === id}
            aria-controls="plugins-panel" tabIndex={tab === id ? 0 : -1}
            onClick={() => navigate({kind: 'list', tab: id}, `#plugins-tab-${id}`)}>{id === 'system' ? p.systemHeading : p.externalHeading}</button>)}
        </div>
        : <button id="plugins-back" type="button" onClick={() => navigate(SYSTEM_LIST, `#${AGENT_DETAILS_CONTROL}`)}>
          <span aria-hidden="true">← </span>{p.back}</button>}
    </div>
    <div className="dialog-body">
      {unavailable && <p className="inline-warning" role="status">{unavailable}</p>}
      {tab === 'system' && <div id="plugins-panel" role="tabpanel" aria-labelledby="plugins-tab-system">
        <p className="muted">{p.systemHelp}</p>
        <ul className="plugin-list">
          <AgentIntegrationEntry state={state} admitted={admitted}
            onDetails={() => navigate({kind: 'detail', plugin: AGENT_INTEGRATION}, '#plugin-agent-heading')}/>
        </ul>
      </div>}
      {tab === 'external' && <div id="plugins-panel" role="tabpanel" aria-labelledby="plugins-tab-external" tabIndex={0}>
        <p className="plugin-unsupported">{p.externalHelp}</p>
      </div>}
      {page.kind === 'detail' && <AgentIntegrationDetail state={state} admitted={admitted} onEdit={session.edit}
        onRefresh={() => read('#plugins-refresh')} onCheck={() => read('#plugin-check')}
        onReview={review} onCancelReview={cancelReview} onConfirm={confirm}/>}
    </div>
    <div className="dialog-footer">
      <span role="status">{status}</span>
      {authoring && <AuthoringFooterStatus key={authoring.activity.owner.token} {...authoring} idPrefix="plugins-authoring"/>}
      <button type="button" id="close-plugins" onClick={onClose}>{ui.common.close}</button>
    </div>
  </Modal>;
}

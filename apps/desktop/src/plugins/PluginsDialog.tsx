import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import Modal from '../components/Modal.tsx';
import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {AuthoringFooterStatus} from '../components/StatusSurface.tsx';
import type {AuthoringStatusProps} from '../components/StatusSurface.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {ACTION_ORDER, canReview, executableChanged, pluginBusy} from './plugin-management.ts';
import type {PluginAction, PluginResult, PluginView} from './plugin-management.ts';
import type {PluginManagementControls} from './usePluginManagement.ts';
import './plugins.css';

interface Props {
  open: boolean; onClose: () => void; plugins: PluginManagementControls;
  // Why management cannot be admitted now (Setup not complete, application closing); null when it can.
  unavailable: string | null;
  strip: ReactNode; authoring: AuthoringStatusProps | null;
}

// Observed facts of one OMP target; a confirmation shows the captured observation it will act on.
function TargetFacts({view}: {view: PluginView}) {
  const p = messages[useLocale()].ui.plugins;
  const compatibility = view.state === 'missing' || view.state === 'unsupported' || view.state === 'incompatible' ? view.state : 'compatible';
  return <dl className="plugin-facts">
    <dt>{p.executable}</dt><dd className="mono">{view.target?.executable ?? p.notFound}</dd>
    <dt>{p.ompVersion}</dt><dd>{view.target ? `${view.target.version} · ` : ''}{p.minimum(view.minimum_omp_version)}</dd>
    <dt>{p.userRoot}</dt><dd className="mono">{view.target?.user_root ?? p.notEstablished}</dd>
    <dt>{p.installed}</dt><dd>{view.installed ? `${view.installed.version} · ${view.installed.managed ? p.managed : p.unmanaged}` : view.state === 'absent' ? p.notInstalled : p.notEstablished}</dd>
    {view.installed && <><dt>{p.source}</dt><dd className="mono">{view.installed.source}</dd></>}
    <dt>{p.included}</dt><dd>{view.included_version}</dd>
    <dt>{p.compatibility}</dt><dd>{p.compatibilityStates[compatibility]}</dd>
  </dl>;
}

function ResultNotice({result}: {result: PluginResult}) {
  const p = messages[useLocale()].ui.plugins;
  const kind = result.error !== null ? 'rejected' : result.outcome?.status ?? 'unknown';
  const heading = p.result(kind, result.action);
  const issue = kind === 'rejected' ? fault(result.error) : result.outcome?.issue ?? null;
  const target = result.target && <dl className="plugin-facts">
    <dt>{p.executable}</dt><dd className="mono">{result.target.executable}</dd>
    <dt>{p.userRoot}</dt><dd className="mono">{result.target.user_root}</dd>
  </dl>;
  if (kind === 'verified') return <div id="plugin-result" className="receipt plugin-result" tabIndex={-1} role="status">
    <strong>{heading}</strong>{target}<p>{p.freshSession[result.action]}</p>
  </div>;
  return <div id="plugin-result" className="plugin-result" tabIndex={-1}>
    {issue ? <FaultMessage title={heading} value={issue}/> : <p className="inline-warning" role="status"><strong>{heading}</strong></p>}
    {target}
    <p className="field-help">{p.resultHelp[kind]}</p>
  </div>;
}

export default function PluginsDialog({open, onClose, plugins: {state, session}, unavailable, strip, authoring}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const p = ui.plugins;
  const [focusRequest, setFocusRequest] = useState<string | null>(null);
  const wasApplying = useRef(state.applying !== null);
  // The read control the operator used last; focus returns to it after its read settles.
  const readControl = useRef('#plugins-refresh');
  // Move focus to a requested control, and keep it inside the dialog when the focused control unmounts.
  useEffect(() => {
    const settled = wasApplying.current && state.applying === null;
    wasApplying.current = state.applying !== null;
    const dialog = document.getElementById('plugins');
    if (!open || !dialog?.hasAttribute('open')) return;
    if (focusRequest !== null) setFocusRequest(null);
    const requested = focusRequest === null ? null : dialog.querySelector<HTMLElement>(focusRequest);
    if (requested && !requested.matches(':disabled')) {
      requested.focus();
      return;
    }
    const active = document.activeElement;
    if (state.inspecting || (active !== null && active !== document.body && active !== dialog && dialog.contains(active))) return;
    // Progress, then a settled read-back result, then the last read control take the lost focus.
    for (const selector of ['#plugin-pending', settled ? '#plugin-result' : null, readControl.current, '#plugins-refresh', '#close-plugins']) {
      const element = selector === null ? null : dialog.querySelector<HTMLElement>(selector);
      if (element && !element.matches(':disabled')) {
        element.focus();
        return;
      }
    }
  });

  const admitted = unavailable === null;
  const busy = pluginBusy(state);
  const view = state.view;
  const confirmation = state.confirmation;
  const applying = state.applying;
  const offered = view ? ACTION_ORDER.filter(action => view.actions.includes(action)) : [];
  const status = applying ? p.running[applying.action] : state.inspecting ? p.inspecting : unavailable ?? '';

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
    onCancel={() => {if (state.confirmation) cancelReview(); else onClose();}} initialFocus="#plugins-refresh:not(:disabled), #plugin-executable">
    <div className="dialog-header"><div><h2 id="plugins-heading">{p.heading}</h2><p>{p.intro}</p></div>
      <button type="button" className="icon" aria-label={p.close} title={p.close} onClick={onClose}>×</button></div>
    {strip}
    <div className="dialog-body">
      {unavailable && <p className="inline-warning">{unavailable}</p>}
      <section className="plugin-section" aria-labelledby="plugins-system-heading">
        <h3 id="plugins-system-heading">{p.systemHeading}</h3>
        <p className="muted">{p.systemHelp}</p>
        <article className="plugin-card" aria-labelledby="plugin-agent-heading">
          <h4 id="plugin-agent-heading">{p.agentHeading}</h4>
          <p className="muted">{p.agentHelp}</p>
          <dl className="plugin-facts">
            <dt>{p.origin}</dt><dd>{p.originSystem}</dd>
            <dt>{p.activation}</dt><dd>{p.activationInstallation}</dd>
          </dl>
          <h5 id="plugin-targets-heading">{p.targets}</h5>
          <ul className="plugin-targets" aria-labelledby="plugin-targets-heading">
            <li className="plugin-target" aria-labelledby="plugin-omp-heading">
              <div className="plugin-target-heading">
                <strong id="plugin-omp-heading">OMP</strong>
                <span className="tag">{p.userScope}</span>
                {admitted && view && <span className={`tag plugin-state-${view.state}`}>{p.state(view.state)}</span>}
                <button id="plugins-refresh" type="button" disabled={!admitted || busy}
                  onClick={() => {readControl.current = '#plugins-refresh'; void session.refresh();}}>{p.refresh}</button>
              </div>
              <div className="field plugin-executable">
                <label htmlFor="plugin-executable">{p.executableLabel}</label>
                <div className="plugin-executable-row">
                  <input id="plugin-executable" type="text" value={state.executable} spellCheck={false} autoComplete="off" autoCapitalize="off"
                    placeholder={p.executablePlaceholder} aria-describedby="plugin-executable-help" onChange={event => session.edit(event.target.value)}/>
                  <button id="plugin-check" type="button" disabled={!admitted || busy}
                    onClick={() => {readControl.current = '#plugin-check'; void session.check();}}>{p.check}</button>
                </div>
                <p id="plugin-executable-help" className="field-help">{p.executableHelp}</p>
              </div>
              {admitted && <>
                {state.inspectError !== null && <><FaultMessage title={p.inspectFailed} value={fault(state.inspectError)}/>
                  <p className="field-help">{p.inspectFailedHelp}</p></>}
                {view === null && state.inspectError === null && <p className="muted">{state.inspecting ? p.inspecting : p.notRead}</p>}
                {view && <>
                  <TargetFacts view={view}/>
                  <details className="plugin-identity"><summary>{p.identity}</summary>
                    <dl className="plugin-facts"><dt>{p.includedContent}</dt><dd className="mono">{view.included_content}</dd>
                      <dt>{p.installedContent}</dt><dd className="mono">{view.installed ? view.installed.content ?? p.unknownContent : view.state === 'absent' ? p.notInstalled : p.notEstablished}</dd></dl>
                  </details>
                  <p className="plugin-state-help">{p.stateHelp(view.state)}</p>
                  {view.issue && <FaultMessage title={p.issue} value={view.issue}/>}
                  {executableChanged(state) && <p className="inline-warning">{p.executableChanged}</p>}
                  {state.stale && <p className="inline-warning">{p.stale}</p>}
                  {state.withdrawn && <p className="inline-warning" role="status">{p.withdrawn}</p>}
                  {offered.length > 0
                    ? <div className="button-row">{offered.map(action => <button key={action} id={`plugin-action-${action}`} type="button"
                      className={action === 'uninstall' ? undefined : 'primary'} disabled={!canReview(state, action)}
                      aria-expanded={confirmation?.action === action} aria-controls={confirmation?.action === action ? 'plugin-confirmation' : undefined}
                      onClick={() => review(action)}>{p.actions[action]}</button>)}</div>
                    : !applying && <p className="muted">{p.noActions}</p>}
                  {confirmation && <section id="plugin-confirmation" className="plugin-confirmation" aria-labelledby="plugin-confirm-heading">
                    <h5 id="plugin-confirm-heading">{p.confirmHeading[confirmation.action]}</h5>
                    <p>{p.confirmBody(confirmation.action, {included: confirmation.view.included_version,
                      installed: confirmation.view.installed?.version ?? p.notInstalled, source: confirmation.view.installed?.source ?? p.notInstalled})}</p>
                    <TargetFacts view={confirmation.view}/>
                    {confirmation.action !== 'install' && <p>{p.sessionDisconnect} <code>{'/mado disconnect {}'}</code></p>}
                    <p className="field-help">{p.confirmScope}</p>
                    <div className="button-row">
                      <button id="plugin-confirm-cancel" type="button" onClick={cancelReview}>{ui.common.cancel}</button>
                      <button id="plugin-confirm" type="button" className={confirmation.action === 'uninstall' ? 'danger-text' : 'primary'}
                        onClick={confirm}>{p.confirm[confirmation.action]}</button>
                    </div>
                  </section>}
                </>}
              </>}
              {applying && <div id="plugin-pending" className="inline-info plugin-pending" tabIndex={-1}>
                <strong>{p.running[applying.action]}</strong>
                <p>{p.pending(applying.view.target?.executable ?? p.notFound)}</p>
              </div>}
              {!applying && state.result && <ResultNotice result={state.result}/>}
            </li>
          </ul>
          <section className="plugin-sessions" aria-labelledby="plugin-sessions-heading">
            <h5 id="plugin-sessions-heading">{p.sessionHeading}</h5>
            <p>{p.sessionHelp}</p>
            <p>{p.sessionDisconnect} <code>{'/mado disconnect {}'}</code></p>
            <p>{p.sessionFresh} <code>/extensions</code></p>
          </section>
        </article>
      </section>
      <section className="plugin-section" aria-labelledby="plugins-external-heading">
        <h3 id="plugins-external-heading">{p.externalHeading}</h3>
        <p className="plugin-unsupported">{p.externalHelp}</p>
      </section>
    </div>
    <div className="dialog-footer">
      <span role="status">{status}</span>
      {authoring && <AuthoringFooterStatus key={authoring.activity.owner.token} {...authoring} idPrefix="plugins-authoring"/>}
      <button type="button" id="close-plugins" onClick={onClose}>{ui.common.close}</button>
    </div>
  </Modal>;
}

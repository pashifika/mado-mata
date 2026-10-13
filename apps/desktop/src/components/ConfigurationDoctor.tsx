import {useEffect} from 'react';
import type {ReactNode} from 'react';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {AuthoringFooterStatus} from './StatusSurface.tsx';
import type {AuthoringStatusProps} from './StatusSurface.tsx';
import type {DoctorCheck, DoctorRoute} from '../doctor-types.ts';
import type {DoctorState} from '../doctor-state.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import './configuration-doctor.css';

export function DoctorContent({state, onRefresh, onRoute, routeBlock}: {
  state: DoctorState;
  onRefresh: () => void;
  onRoute: (check: DoctorCheck, route: DoctorRoute) => void;
  routeBlock: (check: DoctorCheck, route: DoctorRoute) => string | null;
}) {
  const ui = messages[useLocale()].ui;
  const d = ui.doctor;
  const report = state.report;
  const incomplete = report?.truncated || report?.checks.some(check => check.status === 'unexamined');
  const failed = report?.checks.some(check => check.status === 'failed');
  return <section className="doctor-content" aria-label={d.heading}>
    <p className="muted">{d.intro}</p>
    <div className="button-row"><button id="doctor-refresh" type="button" disabled={state.pending} onClick={onRefresh}>{d.refresh}</button>
      <span role="status">{state.pending ? d.loading : report ? incomplete ? d.partial : failed ? d.failed : d.checked : ''}</span></div>
    {state.error && <FaultMessage title={d.requestFailed} value={state.error}/>}
    {report && <>
      {report.truncated && <p className="inline-warning">{d.truncated}</p>}
      <details className="doctor-root"><summary>{d.localDetails}</summary><p className="mono">{report.root ?? ui.bootstrap.rootUnresolved}</p><p className="muted">{d.privacy}</p></details>
      <ul className="doctor-checks">{report.checks.map((check, index) => {
        const owner = [check.owner.internal_name, check.owner.package_id, check.owner.profile_id].filter(Boolean).join(' · ');
        return <li key={index} className={`doctor-check doctor-${check.status}`}>
          <div className="doctor-check-heading"><strong>{d.scopes[check.scope]}</strong><span className="tag">{d.statuses[check.status]}</span></div>
          <p className="mono">{owner || d.applicationOwner}</p>
          <p>{d.codes[check.code]}</p>
          {check.fault_category && <code>{check.fault_category}</code>}
          <details><summary>{d.localDetails}</summary>
            {check.details.path && <p className="mono">{check.details.path}</p>}
            {check.owner.artifact_id && <p className="mono">{check.owner.artifact_id}</p>}
            <p className="muted">{d.privacy}</p>
          </details>
          {check.routes.length > 0 && <div className="doctor-routes"><p className="muted">{d.navigationOnly}</p>
            {check.routes.map(route => {
              if (route === 'manual') return <p key={route}>{d.manual}</p>;
              const blocked = routeBlock(check, route);
              return <div key={route}><p>{d.preservation[route]}</p><button type="button" disabled={blocked !== null} title={blocked ?? undefined}
                onClick={() => onRoute(check, route)}>{d.routes[route]}</button>{blocked && <p className="field-help">{blocked}</p>}</div>;
            })}
          </div>}
        </li>;
      })}</ul>
    </>}
  </section>;
}

export default function ConfigurationDoctor({open, onClose, strip, authoring, children}: {
  open: boolean; onClose: () => void; strip: ReactNode; authoring: AuthoringStatusProps | null; children: ReactNode;
}) {
  const ui = messages[useLocale()].ui;
  useEffect(() => {
    const dialog = document.getElementById('configuration-doctor');
    if (!open || !(dialog instanceof HTMLDialogElement) || !dialog.open) return;
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !dialog.contains(active) || active === dialog || active.matches(':disabled')) document.getElementById('doctor-heading')?.focus();
  });
  return <Modal id="configuration-doctor" open={open} onCancel={onClose} labelledBy="doctor-heading" initialFocus="#doctor-heading"
    returnFocus={() => document.getElementById('bootstrap-doctor') ?? document.getElementById('application-menu')} className="configuration-doctor">
    <div className="dialog-header"><h2 id="doctor-heading" tabIndex={-1}>{ui.doctor.heading}</h2>
      <button type="button" className="icon" aria-label={ui.common.close} onClick={onClose}>×</button></div>
    {strip}
    <div className="dialog-body">{children}</div>
    <div className="dialog-footer">
      {authoring && <AuthoringFooterStatus key={authoring.activity.owner.token} {...authoring} idPrefix="doctor-authoring"/>}
      <button id="doctor-close" type="button" onClick={onClose}>{ui.common.close}</button>
    </div>
  </Modal>;
}

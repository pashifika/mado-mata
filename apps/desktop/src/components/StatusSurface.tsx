import {useLayoutEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {shortRevision} from '../authoring.ts';
import type {AuthoringSession} from '../authoring.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {packageValidation} from '../status.ts';
import type {AuthoringActivity, StatusItem} from '../status.ts';
import type {Fault} from '../types.ts';
import AuthoringValidation from './AuthoringValidation.tsx';
import {HelpPanel, HelpProvider, HelpTrigger, InfoHint, useHelp} from './ContextualHelp.tsx';

export interface AuthoringControlsProps {
  activity: AuthoringActivity; ownerLabel: string; showReturn: boolean; returnDisabled: boolean; stopDisabled: boolean;
  onReturn: () => void; onStop: () => void; onRelease: () => void; details: ReactNode;
}

// The modal equivalent deliberately owns its disclosure inside the focus-contained surface.
export function AuthoringControls({idPrefix, activity, ownerLabel, showReturn, returnDisabled, stopDisabled, onReturn, onStop, onRelease, details,
  onDetails, expanded = false}: AuthoringControlsProps & {idPrefix: string; onDetails?: (opener: HTMLElement) => void; expanded?: boolean}) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const a = ui.authoring;
  const worker = activity.worker;
  const capture = activity.capture && (activity.capture.occupied || activity.capture.busy) ? activity.capture : null;
  const attention = activity.items.length > 0;
  const detailLabel = activity.cleanupIncomplete ? ui.recognition.cleanupIncomplete : attention ? a.activityAttention : a.activityDetails;
  const detailTitle = activity.items.map(item => item.text).join('\n') || a.activityDetails;
  return <div id={`${idPrefix}-authoring-status`} className="authoring-compact">
    <div className="authoring-controls">
      <span className="authoring-owner">
        <strong title={`${a.ownerEyebrow} · ${ownerLabel}`}>{a.ownerEyebrow} · {ownerLabel}</strong>
        <InfoHint id={`${idPrefix}-authoring-help`} label={a.details} hint={a.authority}/>
      </span>
      <span className="authoring-worker" role="status" aria-live="polite">
        {worker ? <><span className="activity-kind" title={worker.kind}>{worker.kind}</span>
          <span className={`phase phase-${worker.phase}`}>{activity.stopRequested ? a.stopPending : ui.phase(worker.phase)}</span></> : !capture && <span className="muted">{ui.phase('idle')}</span>}
      </span>
      {worker?.cancellable && <button id={`${idPrefix}-stop`} type="button" className="stop-button" disabled={stopDisabled || worker.phase === 'stopping'} onClick={onStop}>{messages[locale].app.stop}</button>}
      {capture && <span className="authoring-capture">
        <span className="activity-kind" title={`${ui.nativeCapture.heading} · ${ui.nativeCapture.status[capture.status]}`} role="status">
          {ui.nativeCapture.heading} · {activity.stopRequested && capture.busy ? a.stopPending : ui.nativeCapture.status[capture.status]}
        </span>
        <button id={`${idPrefix}-native-stop`} type="button" className={capture.busy ? 'stop-button' : undefined}
          disabled={stopDisabled || capture.status === 'cancelling'} onClick={capture.busy ? onStop : onRelease}>
          {capture.busy ? messages[locale].app.stop : a.releaseCapture}
        </button>
      </span>}
      {showReturn && <button id={`${idPrefix}-return-to-edit`} type="button" disabled={returnDisabled} onClick={onReturn}>{a.returnToEdit}</button>}
      {onDetails && <button id={`${idPrefix}-activity-details`} type="button" className={attention ? 'status-error' : undefined}
        title={detailTitle} aria-expanded={expanded} aria-controls="workspace-help-panel" onClick={event => onDetails(event.currentTarget)}>
        <span role="status">{detailLabel}{attention ? ` (${activity.items.length})` : ''}</span>
      </button>}
    </div>
    {!onDetails && <details className="authoring-local-details">
      <summary className={attention ? 'status-error' : undefined} title={detailTitle}>{detailLabel}{attention ? ` (${activity.items.length})` : ''}</summary>
      {details}
    </details>}
  </div>;
}

interface Props {
  scopeKey: string; scopeLabel: string; pageTitle: string; pageHelp: ReactNode; runLabel: string;
  items: StatusItem[]; session: AuthoringSession | null; recognitionDirty: boolean; validating: boolean;
  profile: 'valid' | 'none' | 'stale' | null;
  authoring: AuthoringControlsProps | null;
  onNavigate: (action: NonNullable<StatusItem['action']>) => void;
  onDiagnostic: (fault: Fault) => void;
  children: ReactNode;
}

export default function StatusSurface({scopeKey, scopeLabel, pageTitle, pageHelp, runLabel, items, session, recognitionDirty, validating, profile, authoring, onNavigate, onDiagnostic, children}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const [panel, setPanel] = useState<'help' | 'status' | 'validation' | 'authoring'>('help');
  const help = useHelp(`${scopeKey}:${authoring?.activity.owner.token ?? ''}`, () => setPanel('help'));
  const bar = useRef<HTMLElement>(null);
  const drawer = useRef<HTMLDivElement>(null);
  const validation = session ? packageValidation(session, validating) : null;
  const unsaved = (validation?.unsaved ?? 0) + Number(recognitionDirty);
  useLayoutEffect(() => {
    const element = bar.current;
    const shell = element?.closest<HTMLElement>('.app');
    if (!element || !shell) return;
    const page = document.documentElement;
    const previousPadding = page.style.scrollPaddingBottom;
    const measure = () => {
      const statusHeight = element.getBoundingClientRect().height;
      const drawerHeight = drawer.current?.getBoundingClientRect().height ?? 0;
      const helpHeight = drawer.current?.querySelector('.workspace-drawer')?.getBoundingClientRect().height ?? 0;
      shell.style.setProperty('--status-height', `${statusHeight}px`);
      shell.style.setProperty('--drawer-height', `${drawerHeight}px`);
      shell.style.setProperty('--unsaved-height', `${drawerHeight - helpHeight}px`);
      page.style.scrollPaddingBottom = `${statusHeight + drawerHeight + 8}px`;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    if (drawer.current) observer.observe(drawer.current);
    const unsavedStatus = drawer.current?.querySelector('.unsaved-status');
    if (unsavedStatus) observer.observe(unsavedStatus);
    return () => {
      observer.disconnect();
      page.style.scrollPaddingBottom = previousPadding;
    };
  }, [help.topic]);
  const openDetails = (kind: 'status' | 'validation' | 'authoring', opener: HTMLElement) => {
    help.open({title: kind === 'status' ? ui.status.details : kind === 'authoring' ? ui.authoring.activityDetails : ui.authoring.savedValidationHeading, content: null}, opener);
    setPanel(kind);
  };
  const details = panel === 'status' ? <ul className="status-details">{items.map(item => <li key={item.id} className={`status-${item.severity}`}>
    <span>{item.earlierDraft && <>{ui.target.earlier} · </>}{item.text}</span>{item.action && <button type="button" onClick={() => {help.close(); onNavigate(item.action!);}}>
      {item.action === 'execution' ? ui.status.execution : item.action === 'edit' ? messages[locale].app.edit : runLabel}
    </button>}
  </li>)}</ul> : panel === 'authoring' ? authoring?.details : panel === 'validation' && session ? <AuthoringValidation session={session} recognitionDirty={recognitionDirty} validating={validating}
    onNavigate={fault => {help.close(); onDiagnostic(fault);}}/> : undefined;
  return <HelpProvider controller={help}>
    {children}
    <div ref={drawer} className="workspace-feedback">
      {help.topic && <div className="workspace-drawer">
        <HelpPanel controller={help} title={panel === 'status' ? ui.status.details : panel === 'authoring' ? ui.authoring.activityDetails : panel === 'validation' ? ui.authoring.savedValidationHeading : undefined}>{details}</HelpPanel>
      </div>}
      <div className="unsaved-status" role="status">{unsaved > 0 && <span id="status-unsaved" className="unsaved-bubble" title={ui.authoring.unsavedNotValidated}>{ui.authoring.unsavedFiles(unsaved)}</span>}</div>
    </div>
    <footer ref={bar} id="workspace-status" className={`status-bar${authoring ? ' status-with-authoring' : ''}`} aria-label={ui.status.heading}>
      {authoring && <AuthoringControls {...authoring} idPrefix="app" onDetails={opener => openDetails('authoring', opener)}
        expanded={help.topic !== null && panel === 'authoring'}/>}
      <div className="status-context">
      <strong className="status-scope" title={scopeLabel}>{scopeLabel}</strong>
      <button id="status-details" className={`status-message status-${items[0]?.severity ?? 'info'}`} type="button"
        aria-expanded={help.topic !== null && panel === 'status'} aria-controls="workspace-help-panel"
        onClick={event => openDetails('status', event.currentTarget)}>
        <span role="status" aria-live="polite">{items[0] ? <>{items[0].earlierDraft && <>{ui.target.earlier} · </>}{items[0].text}</> : ui.status.ready}</span>
      </button>
      <div className="status-facts">
        {profile !== null && <span id="profile-validation-status" className={`tag ${profile === 'valid' ? 'current' : ''}`}>{ui.status.profile} · {ui.status[profile]}</span>}
        {validation && <button id="package-validation-status" type="button" className={`validation-summary ${validation.state === 'invalid' ? 'status-error' : validation.state === 'stale' ? 'status-warning' : ''}`}
          aria-expanded={help.topic !== null && panel === 'validation'} aria-controls="workspace-help-panel"
          onClick={event => openDetails('validation', event.currentTarget)}>
          {ui.authoring.savedValidationHeading} · {validation.state === 'running' ? ui.authoring.validating : ui.status[validation.state]}
          {validation.revision !== null && <> · {shortRevision(validation.revision)} · {ui.authoring.validationDiagnosticCount(validation.count)}</>}
        </button>}
        <HelpTrigger id="workspace-help" label={ui.status.help} title={pageTitle} hint={ui.status.helpHint}>{pageHelp}</HelpTrigger>
      </div>
      </div>
    </footer>
  </HelpProvider>;
}

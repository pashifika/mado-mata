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

export interface AuthoringStatusProps {
  activity: AuthoringActivity; ownerLabel: string; showReturn: boolean; returnDisabled: boolean; stopDisabled: boolean;
  onReturn: () => void; onStop: () => void; onRelease: () => void; details: ReactNode;
}

function AuthoringActions({idPrefix, activity, ownerLabel, showReturn, returnDisabled, stopDisabled, onReturn, onStop, onRelease}: AuthoringStatusProps & {idPrefix: string}) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const a = ui.authoring;
  const worker = activity.worker;
  const capture = activity.capture && (activity.capture.occupied || activity.capture.busy) ? activity.capture : null;
  return <>
    {worker?.cancellable && <button id={`${idPrefix}-stop`} type="button" className="stop-button" aria-label={`${messages[locale].app.stop} · ${ownerLabel}`}
      title={`${messages[locale].app.stop} · ${ownerLabel}`} disabled={stopDisabled || worker.phase === 'stopping'} onClick={onStop}>{messages[locale].app.stop}</button>}
    {capture && <span className="authoring-capture">
      <span className="activity-kind" title={`${ui.nativeCapture.heading} · ${ui.nativeCapture.status[capture.status]}`} role="status">
        {ui.nativeCapture.heading} · {activity.stopRequested && capture.busy ? a.stopPending : ui.nativeCapture.status[capture.status]}
      </span>
      <button id={`${idPrefix}-native-stop`} type="button" className={capture.busy ? 'stop-button' : undefined}
        aria-label={`${capture.busy ? messages[locale].app.stop : a.releaseCapture} · ${ownerLabel}`}
        title={`${capture.busy ? messages[locale].app.stop : a.releaseCapture} · ${ownerLabel}`} disabled={stopDisabled || capture.status === 'cancelling'} onClick={capture.busy ? onStop : onRelease}>
        {capture.busy ? messages[locale].app.stop : a.releaseCapture}
      </button>
    </span>}
    {showReturn && <button id={`${idPrefix}-return-to-edit`} type="button" disabled={returnDisabled}
      aria-label={`${a.returnToEdit} · ${ownerLabel}`} title={`${a.returnToEdit} · ${ownerLabel}`} onClick={onReturn}>{a.returnToEdit}</button>}
  </>;
}

export function AuthoringFooterStatus({idPrefix, activity, ownerLabel, returnDisabled, stopDisabled, onReturn, onStop, onRelease, details}: AuthoringStatusProps & {idPrefix: string}) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const a = ui.authoring;
  const worker = activity.worker;
  const capture = activity.capture && (activity.capture.occupied || activity.capture.busy) ? activity.capture : null;
  const attention = activity.cleanupIncomplete ? ui.recognition.cleanupIncomplete : activity.items.length > 0 ? a.activityAttention : null;
  const authoring = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = authoring.current;
    const dialog = element?.closest('dialog');
    const footer = element?.closest('.dialog-footer');
    if (!attention || !element || !dialog || !footer) return;
    const measure = () => {
      // Leave room for the panel's upward offset and the dialog's top border.
      const available = footer.getBoundingClientRect().top - dialog.getBoundingClientRect().top - 24;
      element.style.setProperty('--authoring-details-max-height', `${Math.max(0, Math.min(window.innerHeight * 0.4, available))}px`);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(dialog);
    observer.observe(footer);
    window.addEventListener('resize', measure);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
      element.style.removeProperty('--authoring-details-max-height');
    };
  }, [attention]);
  const phase = activity.stopRequested ? 'stopping' : worker?.phase ?? (capture?.busy ? 'running' : 'idle');
  const stateLabel = activity.stopRequested ? a.stopPending : worker ? ui.phase(worker.phase) : capture ? ui.nativeCapture.status[capture.status] : ui.phase('idle');
  return <div ref={authoring} id={`${idPrefix}-status`} className="footer-authoring">
    <button id={`${idPrefix}-return-to-edit`} type="button" className={`tag edit-status phase-${phase} ${attention ? 'status-attention' : ''}`}
      disabled={returnDisabled} title={`${a.returnToEdit} · ${ownerLabel}${worker ? ` · ${worker.kind}` : ''}`}
      aria-label={`${a.returnToEdit} · ${ownerLabel}${worker ? ` · ${worker.kind}` : capture ? ` · ${ui.nativeCapture.heading}` : ''} · ${stateLabel}`} onClick={onReturn}>
      <span>{a.ownerEyebrow} · {stateLabel}</span>
    </button>
    {worker?.cancellable && <button id={`${idPrefix}-stop`} type="button" className="stop-button" aria-label={`${messages[locale].app.stop} · ${ownerLabel}`}
      title={`${messages[locale].app.stop} · ${ownerLabel}`} disabled={stopDisabled || worker.phase === 'stopping'} onClick={onStop}>{messages[locale].app.stop}</button>}
    {capture && <button id={`${idPrefix}-native-stop`} type="button" className={capture.busy ? 'stop-button' : undefined}
      aria-label={`${capture.busy ? messages[locale].app.stop : a.releaseCapture} · ${ownerLabel}`}
      title={`${capture.busy ? messages[locale].app.stop : a.releaseCapture} · ${ownerLabel}`} disabled={stopDisabled || capture.status === 'cancelling'} onClick={capture.busy ? onStop : onRelease}>
      {capture.busy ? messages[locale].app.stop : a.releaseCapture}
    </button>}
    {attention && <details id={`${idPrefix}-details`} className="footer-authoring-details" onKeyDown={event => {
      if (event.key === 'Escape' && !event.defaultPrevented && event.currentTarget.open) {
        event.preventDefault(); event.stopPropagation(); event.currentTarget.open = false;
        event.currentTarget.querySelector('summary')?.focus();
      }
    }}>
      <summary className="status-error" aria-label={`${ui.status.details} · ${attention}`} title={ui.status.details}>{attention}</summary>
      <div className="footer-authoring-panel">{details}</div>
    </details>}
  </div>;
}

interface Props {
  scopeKey: string; scopeLabel: string; scopePhase: string | null; pageTitle: string; pageHelp: ReactNode; runLabel: string;
  items: StatusItem[]; session: AuthoringSession | null; recognitionDirty: boolean; validating: boolean;
  profile: 'valid' | 'none' | 'stale' | null;
  authoring: AuthoringStatusProps | null;
  onNavigate: (action: NonNullable<StatusItem['action']>) => void;
  onDiagnostic: (fault: Fault) => void;
  children: ReactNode;
}

export default function StatusSurface({scopeKey, scopeLabel, scopePhase, pageTitle, pageHelp, runLabel, items, session, recognitionDirty, validating, profile, authoring, onNavigate, onDiagnostic, children}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const [panel, setPanel] = useState<'help' | 'status' | 'validation'>('help');
  const help = useHelp(`${scopeKey}:${authoring?.activity.owner.token ?? ''}`, () => setPanel('help'));
  const bar = useRef<HTMLElement>(null);
  const drawer = useRef<HTMLDivElement>(null);
  const validation = session ? packageValidation(session, validating) : null;
  const unsaved = (validation?.unsaved ?? 0) + Number(recognitionDirty);
  const activity = authoring?.activity;
  const worker = activity?.worker;
  const capture = activity?.capture && (activity.capture.occupied || activity.capture.busy) ? activity.capture : null;
  const phase = activity ? activity.stopRequested ? 'stopping' : worker?.phase ?? (capture?.busy ? 'running' : 'idle') : scopePhase;
  const stateLabel = activity?.stopRequested ? ui.authoring.stopPending : worker ? ui.phase(worker.phase)
    : capture ? ui.nativeCapture.status[capture.status] : phase === null ? null : ui.phase(phase);
  const message = items[0];
  const messageText = message?.text ?? (worker || capture?.busy ? '' : ui.status.ready);
  const feedbackScope = authoring && !session ? scopeLabel : null;
  const attention = Boolean(activity?.items.length) || items.some(item => item.severity === 'error' || item.severity === 'warning');
  const detailLabel = activity?.cleanupIncomplete ? ui.recognition.cleanupIncomplete : attention ? ui.authoring.activityAttention : ui.status.details;
  const detailTitle = [...activity?.items ?? [], ...items].map(item => item.text).join('\n') || ui.status.details;
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
  const openDetails = (kind: 'status' | 'validation', opener: HTMLElement) => {
    help.open({title: kind === 'status' ? ui.status.details : ui.authoring.savedValidationHeading, content: null}, opener);
    setPanel(kind);
  };
  const details = panel === 'status' ? <>
    {authoring && <section aria-label={ui.authoring.activityDetails}>{authoring.details}</section>}
    <section aria-label={scopeLabel}>
      <h3>{scopeLabel}</h3>
      <ul className="status-details">{items.map(item => <li key={item.id} className={`status-${item.severity}`}>
        <span>{item.earlierDraft && <>{ui.target.earlier} · </>}{item.text}</span>{item.action && <button type="button" onClick={() => {help.close(); onNavigate(item.action!);}}>
          {item.action === 'execution' ? ui.status.execution : item.action === 'edit' ? messages[locale].app.edit : runLabel}
        </button>}
      </li>)}</ul>
      {items.length === 0 && <p className="muted">{ui.status.ready}</p>}
    </section>
  </> : panel === 'validation' && session ? <AuthoringValidation session={session} recognitionDirty={recognitionDirty} validating={validating}
    onNavigate={fault => {help.close(); onDiagnostic(fault);}}/> : undefined;
  return <HelpProvider controller={help}>
    {children}
    <div ref={drawer} className="workspace-feedback">
      {help.topic && <div className="workspace-drawer">
        <HelpPanel controller={help} title={panel === 'status' ? ui.status.details : panel === 'validation' ? ui.authoring.savedValidationHeading : undefined}>{details}</HelpPanel>
      </div>}
      <div className="unsaved-status" role="status">{unsaved > 0 && <span id="status-unsaved" className="unsaved-bubble" title={ui.authoring.unsavedNotValidated}>{ui.authoring.unsavedFiles(unsaved)}</span>}</div>
    </div>
    <footer ref={bar} id="workspace-status" className="status-bar" aria-label={ui.status.heading}>
      <div className="status-context">
        <span className="status-identity">
          <strong title={authoring ? `${ui.authoring.ownerEyebrow} · ${authoring.ownerLabel}` : `${pageTitle} · ${scopeLabel}`}>
            {authoring ? ui.authoring.ownerEyebrow : pageTitle}{stateLabel !== null && ' ·'}
          </strong>
          {stateLabel !== null && <span className={`phase phase-${phase}`} role="status">{stateLabel}</span>}
          {authoring && <InfoHint id="app-authoring-help" label={`${ui.authoring.details} · ${authoring.ownerLabel}`}
            hint={`${ui.authoring.ownerEyebrow} · ${authoring.ownerLabel}\n${ui.authoring.authority}`}/>}
        </span>
        <span className="status-message-group" role="status" aria-live="polite">
          {worker && <span className="activity-kind" title={worker.kind}>{worker.kind}</span>}
          <span className={`status-message status-${message?.severity ?? 'info'}`} title={`${scopeLabel} · ${messageText}`}>
            {feedbackScope && messageText && <>{feedbackScope} · </>}{message?.earlierDraft && <>{ui.target.earlier} · </>}{messageText}
          </span>
        </span>
        {authoring && <AuthoringActions {...authoring} idPrefix="app"/>}
        <button id="status-details" type="button" className={attention ? 'status-error' : undefined}
          title={detailTitle} aria-expanded={help.topic !== null && panel === 'status'} aria-controls="workspace-help-panel"
          onClick={event => openDetails('status', event.currentTarget)}>
          <span role="status">{detailLabel}</span>
        </button>
      </div>
      <div className="status-facts">
        {profile !== null && <span id="profile-validation-status" className={`tag ${profile === 'valid' ? 'current' : profile === 'stale' ? 'stale' : ''}`}>{ui.status.profile} · {ui.status[profile]}</span>}
        {validation && <button id="package-validation-status" type="button" className={`validation-summary tag ${validation.state === 'valid' ? 'current' : validation.state === 'invalid' ? 'status-attention' : validation.state === 'stale' ? 'stale' : ''}`}
          title={`${ui.authoring.savedValidationHeading} · ${validation.revision === null ? ui.status.none : `${shortRevision(validation.revision)} · ${ui.authoring.validationDiagnosticCount(validation.count)}`}`}
          aria-expanded={help.topic !== null && panel === 'validation'} aria-controls="workspace-help-panel"
          onClick={event => openDetails('validation', event.currentTarget)}>
          {ui.status.savedPackage} · {validation.state === 'running' ? ui.authoring.validating : ui.status[validation.state]}
        </button>}
      </div>
      <HelpTrigger id="workspace-help" label={ui.status.help} title={pageTitle} hint={ui.status.helpHint}>{pageHelp}</HelpTrigger>
    </footer>
  </HelpProvider>;
}

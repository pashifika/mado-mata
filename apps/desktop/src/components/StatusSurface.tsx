import {useLayoutEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {shortRevision} from '../authoring.ts';
import type {AuthoringSession} from '../authoring.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {packageValidation} from '../status.ts';
import type {StatusItem} from '../status.ts';
import type {Fault} from '../types.ts';
import AuthoringValidation from './AuthoringValidation.tsx';
import {HelpPanel, HelpProvider, HelpTrigger, useHelp} from './ContextualHelp.tsx';

interface Props {
  scopeKey: string; scopeLabel: string; pageTitle: string; pageHelp: ReactNode; runLabel: string;
  items: StatusItem[]; session: AuthoringSession | null; recognitionDirty: boolean; validating: boolean;
  profile: 'valid' | 'none' | 'stale' | null;
  onNavigate: (action: NonNullable<StatusItem['action']>) => void;
  onDiagnostic: (fault: Fault) => void;
  children: ReactNode;
}

export default function StatusSurface({scopeKey, scopeLabel, pageTitle, pageHelp, runLabel, items, session, recognitionDirty, validating, profile, onNavigate, onDiagnostic, children}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const [panel, setPanel] = useState<'help' | 'status' | 'validation'>('help');
  const help = useHelp(scopeKey, () => setPanel('help'));
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
      shell.style.setProperty('--status-height', `${statusHeight}px`);
      shell.style.setProperty('--drawer-height', `${drawerHeight}px`);
      page.style.scrollPaddingBottom = `${statusHeight + drawerHeight + 8}px`;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    if (drawer.current) observer.observe(drawer.current);
    return () => {
      observer.disconnect();
      page.style.scrollPaddingBottom = previousPadding;
    };
  }, [help.topic]);
  const openDetails = (kind: 'status' | 'validation', opener: HTMLElement) => {
    help.open({title: kind === 'status' ? ui.status.details : ui.authoring.savedValidationHeading, content: null}, opener);
    setPanel(kind);
  };
  const details = panel === 'status' ? <ul className="status-details">{items.map(item => <li key={item.id} className={`status-${item.severity}`}>
    <span>{item.earlierDraft && <>{ui.target.earlier} · </>}{item.text}</span>{item.action && <button type="button" onClick={() => {help.close(); onNavigate(item.action!);}}>
      {item.action === 'execution' ? ui.status.execution : item.action === 'edit' ? messages[locale].app.edit : runLabel}
    </button>}
  </li>)}</ul> : panel === 'validation' && session ? <AuthoringValidation session={session} recognitionDirty={recognitionDirty} validating={validating}
    onNavigate={fault => {help.close(); onDiagnostic(fault);}}/> : undefined;
  return <HelpProvider controller={help}>
    {children}
    <div ref={drawer} className="workspace-feedback">
      {help.topic && <div className="workspace-drawer">
        <HelpPanel controller={help} title={panel === 'status' ? ui.status.details : panel === 'validation' ? ui.authoring.savedValidationHeading : undefined}>{details}</HelpPanel>
      </div>}
      {unsaved > 0 && <span id="status-unsaved" className="unsaved-bubble" title={ui.authoring.unsavedNotValidated}>{ui.authoring.unsavedFiles(unsaved)}</span>}
    </div>
    <footer ref={bar} id="workspace-status" className="status-bar" aria-label={ui.status.heading}>
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
    </footer>
  </HelpProvider>;
}

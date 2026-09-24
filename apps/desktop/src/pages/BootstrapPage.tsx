import type {ReactNode} from 'react';
import Select from '../components/Select.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import {reconstructionBlock, restoreBlock} from '../bootstrap.ts';
import type {Admission, BootstrapEvent, BootstrapUi} from '../bootstrap.ts';
import type {BootstrapStatus} from '../types.ts';
import {messages} from '../i18n.ts';
import type {Locale} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export interface BootstrapHandlers {
  onInitialize: () => void; onRetry: () => void; onImportRoot: () => void;
  onSnapshot: () => void; onRestore: () => void; onRecover: (rollback: boolean) => void; onExit: () => void; onDismiss: () => void;
}

interface Props {
  ui: BootstrapUi; status: BootstrapStatus; dispatch: (event: BootstrapEvent) => void; handlers: BootstrapHandlers;
  admission: Admission; exiting: boolean;
  // Inside the Ready shell the Application keeps running; presentation choice and Initialize are not offered there.
  inShell: boolean;
  strip: ReactNode;
}

function Facts({status}: {status: BootstrapStatus}) {
  const t = messages[useLocale()].ui.bootstrap;
  return <dl className="run-identity bootstrap-facts">
    <dt>{t.stateLabel}</dt><dd>{t.state(status.state)}</dd>
    <dt>{t.stage}</dt><dd><code>{status.stage}</code></dd>
    <dt>{t.root}</dt><dd className="mono">{status.root ?? t.rootUnresolved}</dd>
  </dl>;
}

function Receipt({ui}: {ui: BootstrapUi}) {
  const t = messages[useLocale()].ui.bootstrap;
  const outcome = ui.snapshotOutcome;
  if (ui.snapshotPending) return <p className="muted" role="status">{t.snapshotting}</p>;
  if (outcome === null) return null;
  if (outcome.kind === 'fault') return <FaultMessage title={t.snapshotFailed} value={outcome.fault}/>;
  const receipt = outcome.receipt;
  return <div className="receipt" role="status"><strong>{t.snapshotWritten}</strong>
    <dl className="run-identity"><dt>{t.receiptPath}</dt><dd className="mono">{receipt.path}</dd>
      <dt>{t.receiptGeneration}</dt><dd><code>{receipt.generation}</code></dd>
      <dt>{t.receiptFiles}</dt><dd>{receipt.files}</dd><dt>{t.receiptBytes}</dt><dd>{receipt.bytes}</dd></dl></div>;
}

// Setup and Recovery are root-level surfaces. The host status is authoritative: its stage and cause stay visible
// through form edits, failed actions and notices until an authoritative action replaces the status.
export default function BootstrapPage({ui, status, dispatch, handlers, admission, exiting, inShell, strip}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const b = t.bootstrap;
  const setup = status.state === 'setup';
  const ready = status.state === 'ready';
  const discardRequired = status.application_available || admission.anyDirty;
  const Content = inShell ? 'section' : 'main';
  const pending = ui.pending !== null;
  const legacy = status.legacy_root;
  const initializeBlocked = pending || exiting || (legacy !== null && !ui.setup.startFresh);
  const reconstruction = reconstructionBlock(status, admission, ui.restore.discard);
  const restore = restoreBlock(status, ui.restore, admission);
  const receiptGeneration = ui.receiptGeneration;
  const languageOptions = [{value: 'en', label: t.settings.languageNames.en}, {value: 'ja', label: t.settings.languageNames.ja}];
  const asLocale = (value: string, apply: (locale: Locale) => void) => {if (value === 'en' || value === 'ja') apply(value);};
  const snapshotSection = <section className="panel" aria-labelledby="snapshot-heading"><div className="panel-body">
    <h2 id="snapshot-heading">{b.snapshotHeading}</h2>
    <p className="muted">{b.snapshotHelp}</p>
    <div className="field"><label htmlFor="snapshot-destination">{b.destinationOverride}</label>
      <input id="snapshot-destination" type="text" value={ui.snapshotDestination} spellCheck={false} disabled={ui.snapshotPending}
        onChange={event => dispatch({type: 'snapshotDestination', value: event.target.value})}/>
      <p className="field-help">{b.destinationHelp}</p></div>
    <div className="button-row"><button id="bootstrap-snapshot" type="button" disabled={ui.snapshotPending || exiting} onClick={handlers.onSnapshot}>{b.snapshotNow}</button></div>
    <Receipt ui={ui}/>
  </div></section>;
  const restoreSection = <section className="panel" aria-labelledby="restore-heading"><div className="panel-body">
    <h2 id="restore-heading">{b.restoreHeading}</h2>
    <p className="muted">{b.restoreHelp}</p>
    <div className="field"><label htmlFor="archive-path">{b.archivePath}</label>
      <input id="archive-path" type="text" value={ui.restore.archivePath} spellCheck={false} disabled={pending} placeholder={b.archivePlaceholder}
        onChange={event => dispatch({type: 'restore', draft: {...ui.restore, archivePath: event.target.value}})}/></div>
    <dl className="run-identity"><dt>{b.preimage}</dt><dd>{receiptGeneration ? <code>{receiptGeneration}</code> : b.preimageNone}</dd></dl>
    <div className="switch-row"><label htmlFor="restore-confirm">{b.restoreConfirm}</label>
      <input id="restore-confirm" type="checkbox" checked={ui.restore.confirm} disabled={pending} onChange={event => dispatch({type: 'restore', draft: {...ui.restore, confirm: event.target.checked}})}/></div>
    {discardRequired && <div className="switch-row"><label htmlFor="restore-discard">{b.discardConfirm}</label>
      <input id="restore-discard" type="checkbox" checked={ui.restore.discard} disabled={pending} onChange={event => dispatch({type: 'restore', draft: {...ui.restore, discard: event.target.checked}})}/></div>}
    <div className="button-row"><button id="bootstrap-restore" type="button" className="primary" disabled={pending || exiting || restore !== null} onClick={handlers.onRestore}>{b.restore}</button>
      <span className="muted" role="status">{restore ? b.block(restore) : ''}</span></div>
  </div></section>;
  return <div className={inShell ? 'bootstrap-inline' : 'bootstrap-page'}>
    {!inShell && <header className="topbar">
      <div className="brand"><span className="brandmark" aria-hidden="true">M</span><span>MadoMata</span><span className="divider" aria-hidden="true"/><span className="eyebrow">{b.state(status.state)}</span></div>
      <div className="topbar-actions"><div className="inline-label"><label htmlFor="presentation-locale">{b.presentation}</label>
        <Select id="presentation-locale" value={ui.presentation} options={languageOptions} onChange={value => asLocale(value, next => dispatch({type: 'presentation', locale: next}))}/></div>
        <button id="bootstrap-exit" type="button" disabled={exiting} onClick={handlers.onExit}>{exiting ? b.exiting : b.exit}</button></div>
    </header>}
    {strip}
    <Content className="content bootstrap-content" aria-labelledby="bootstrap-heading">
      <div className="page-heading"><div><span className="eyebrow">{b.state(status.state)}</span><h1 id="bootstrap-heading">{setup ? b.setupHeading : ready ? b.restoreHeading : b.recoveryHeading}</h1>
        <p>{setup ? b.setupIntro : ready ? b.restoreHelp : inShell ? b.laterFault : b.recoveryIntro}</p></div>
        {ready && inShell && <button type="button" onClick={handlers.onDismiss}>{t.common.close}</button>}
      </div>
      {!inShell && <p className="field-help">{b.presentationHelp}</p>}
      {inShell && status.settings === null && <div className="inline-label"><label htmlFor="recovery-presentation">{b.presentation}</label>
        <Select id="recovery-presentation" value={ui.presentation} options={languageOptions} onChange={value => asLocale(value, next => dispatch({type:'presentation', locale:next}))}/></div>}
      <section className="panel" aria-label={b.stateLabel}><div className="panel-body">
        <Facts status={status}/>
        {status.fault && <><h3>{b.cause}</h3><FaultMessage title={b.cause} value={status.fault}/><p className="muted">{b.causeHelp}</p><p className="muted">{b.repairHelp}</p></>}
        {!status.application_available && <p className="inline-warning">{b.applicationUnavailable}</p>}
        {ui.actionError && <><FaultMessage title={b.actionFailed} value={ui.actionError}/>
          <div className="button-row"><button type="button" onClick={() => dispatch({type: 'dismissActionError'})}>{t.common.close}</button><span className="muted">{b.actionFailedHelp}</span></div></>}
        <p className="field-help">{b.disclosure}</p>
      </div></section>
      {status.pending_restore && <section className="panel" aria-labelledby="pending-heading"><div className="panel-body">
        <h2 id="pending-heading">{b.pendingRestore}</h2><p className="muted">{b.pendingRestoreHelp}</p>
        <div className="switch-row"><label htmlFor="recover-confirm">{b.recoverConfirm}</label>
          <input id="recover-confirm" type="checkbox" checked={ui.restore.recoverConfirm} disabled={pending} onChange={event => dispatch({type: 'restore', draft: {...ui.restore, recoverConfirm: event.target.checked}})}/></div>
        {discardRequired && <div className="switch-row"><label htmlFor="recover-discard">{b.discardConfirm}</label>
          <input id="recover-discard" type="checkbox" checked={ui.restore.discard} disabled={pending} onChange={event => dispatch({type: 'restore', draft: {...ui.restore, discard: event.target.checked}})}/></div>}
        <div className="button-row">
          <button id="recover-complete" type="button" className="primary" disabled={pending || exiting || !ui.restore.recoverConfirm || reconstruction !== null} onClick={() => handlers.onRecover(false)}>{b.complete}</button>
          <button id="recover-rollback" type="button" className="danger-text" disabled={pending || exiting || !ui.restore.recoverConfirm || reconstruction !== null} onClick={() => handlers.onRecover(true)}>{b.rollback}</button>
          <span className="muted" role="status">{reconstruction ? b.block(reconstruction) : ''}</span></div>
      </div></section>}
      {setup && !status.pending_restore && <section className="panel" aria-labelledby="setup-form-heading"><div className="panel-body">
        <h2 id="setup-form-heading">{b.initialize}</h2>
        <div className="two-col">
          <div className="field"><label htmlFor="setup-locale">{b.savedLanguage}</label>
            <Select id="setup-locale" value={ui.setup.locale} options={languageOptions} disabled={pending} onChange={value => asLocale(value, next => dispatch({type: 'setup', draft: {...ui.setup, locale: next}}))}/>
            <p className="field-help">{b.savedLanguageHelp}</p></div>
          <div className="field"><label htmlFor="setup-backup">{b.backupDirectory}</label>
            <input id="setup-backup" type="text" value={ui.setup.backupDirectory} spellCheck={false} disabled={pending} onChange={event => dispatch({type: 'setup', draft: {...ui.setup, backupDirectory: event.target.value}})}/>
            <p className="field-help">{b.backupDirectoryHelp}</p></div>
        </div>
        {legacy !== null && <div className="legacy-box">
          <h3>{b.legacyHeading}</h3><p className="muted">{b.legacyHelp(legacy)}</p>
          <div className="button-row"><button id="import-legacy-root" type="button" disabled={pending || exiting} onClick={handlers.onImportRoot}>{b.import}</button></div>
          <div className="switch-row"><label htmlFor="start-fresh">{b.startFresh}</label>
            <input id="start-fresh" type="checkbox" checked={ui.setup.startFresh} disabled={pending} onChange={event => dispatch({type: 'setup', draft: {...ui.setup, startFresh: event.target.checked}})}/></div>
          <p className="field-help">{b.startFreshHelp}</p>
        </div>}
        <div className="button-row"><button id="initialize" type="button" className="primary" disabled={initializeBlocked} onClick={handlers.onInitialize}>{b.initialize}</button>
          <span className="muted">{b.initializeHelp}</span></div>
      </div></section>}
      {!setup && !ready && !status.pending_restore && <section className="panel" aria-labelledby="retry-heading"><div className="panel-body">
        <h2 id="retry-heading">{b.retry}</h2><p className="muted">{b.retryHelp}</p>
        {discardRequired && <div className="switch-row"><label htmlFor="retry-discard">{b.discardConfirm}</label>
          <input id="retry-discard" type="checkbox" checked={ui.restore.discard} disabled={pending} onChange={event => dispatch({type: 'restore', draft: {...ui.restore, discard: event.target.checked}})}/></div>}
        <div className="button-row"><button id="retry-bootstrap" type="button" className="primary" disabled={pending || exiting || reconstruction !== null} onClick={handlers.onRetry}>{b.retry}</button>
          <span className="muted" role="status">{reconstruction ? b.block(reconstruction) : ''}</span></div>
      </div></section>}
      {setup ? <details className="bootstrap-advanced"><summary>{b.snapshotHeading} · {b.restoreHeading}</summary>{snapshotSection}{restoreSection}</details> : <>{snapshotSection}{restoreSection}</>}
    </Content>
  </div>;
}

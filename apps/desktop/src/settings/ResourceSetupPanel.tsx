import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import type {OcrSetupControls} from './useOcrSetup.ts';

export default function ResourceSetupPanel({setup, block}:{setup:OcrSetupControls; block:string|null}) {
  const locale = useLocale();
  const t = messages[locale].ui.ocrSetup;
  const {state, busy, stale, proposal} = setup;
  const operation = state.operation;
  const progress = operation?.progress;
  const success = operation !== null && !operation.active && progress?.stage === 'complete' && !operation.error && !operation.cleanup_error;
  const downloaded = state.owned && success && state.resourceId !== 'inspect';
  const blocked = block !== null || busy || state.catalogPending;
  const bytes = new Intl.NumberFormat(locale);
  return <section className="ocr-setup" aria-labelledby="ocr-setup-heading">
    <h4 id="ocr-setup-heading">{t.heading}</h4>
    <p className="muted">{t.introduction}</p>
    {state.catalogPending && <p role="status" className="muted">{t.loading}</p>}
    {(busy || operation) && <div className="ocr-setup-progress" role="status">
      <strong>{state.cancelRequested && busy && operation?.active !== false ? t.cancelling : state.starting ? t.starting : progress ? t.stage(progress.stage) : t.awaitingProgress}</strong>
      {state.id && <code className="ocr-setup-id">{state.id}</code>}
      {operation?.active && progress && <>
        <progress aria-label={t.progress} value={progress.total > 0 ? Math.min(progress.bytes, progress.total) : undefined} max={progress.total || 1}/>
        {progress.total > 0 && <span className="mono">{bytes.format(progress.bytes)} / {bytes.format(progress.total)} {t.bytes}</span>}
      </>}
      {busy && state.owned && operation?.active !== false && <button type="button" disabled={state.cancelPending || (state.cancelRequested && state.error === null)} onClick={setup.cancel}>{t.cancel}</button>}
    </div>}
    {operation && !state.owned && <p className="inline-info">{t.previous}</p>}
    {state.error !== null && <FaultMessage title={t.failed} value={fault(state.error)}/>}
    {state.pollError !== null && <FaultMessage title={t.progressFailed} value={fault(state.pollError)}/>}
    {state.pollError !== null && state.id === null && <button type="button" disabled={state.catalogPending} onClick={setup.reload}>{t.retryStatus}</button>}
    {state.unavailable && <p className="inline-warning" role="alert">{t.unavailable}</p>}
    {operation?.error && <FaultMessage title={progress?.stage === 'cancelled' ? t.cancelled : t.failed} value={operation.error}/>}
    {operation?.cleanup_error && <><FaultMessage title={t.cleanupFailed} value={operation.cleanup_error}/><p className="inline-warning">{t.cleanupHelp}</p></>}
    {downloaded && <p className="inline-info">{t.downloaded}</p>}
    {state.owned && success && state.resourceId === 'inspect' && operation?.result === null && <p className="inline-warning" role="alert">{t.noResult}</p>}
    {state.view && <ul className="ocr-setup-items">
      {state.view.items.map(item => {
        const owned = state.resourceId === item.id;
        const verifiedDownload = owned && downloaded;
        const itemStatus = owned && busy ? t.stage(progress?.stage ?? 'resolving') : verifiedDownload ? t.downloadedState : t.state(item.state);
        return <li key={item.id} className="ocr-setup-item">
          <div className="section-heading"><strong>{item.name[locale]}</strong>
            <span className={`tag ${verifiedDownload || item.state === 'verified' ? 'current' : item.state === 'incompatible' || item.state === 'unsupported' ? 'stale' : ''}`}>{itemStatus}</span></div>
          <p className="field-help">{item.detail[locale]}</p>
          <div className="button-row">
            {item.downloadable && <button type="button" disabled={blocked || verifiedDownload} onClick={() => setup.start(item.id)}>{t.download}</button>}
            {item.links.map((link, index) => <button type="button" className="link" key={index} disabled={state.guidancePending !== null}
              onClick={() => setup.guidance('link', item.id, index)} title={link.url}>{link.label[locale]} ↗</button>)}
          </div>
          {item.commands.map((command, index) => <div className="ocr-setup-command" key={index}>
            <pre>{command}</pre><button type="button" disabled={state.guidancePending !== null}
              aria-label={`${t.copy}: ${item.name[locale]}`} onClick={() => setup.guidance('copy', item.id, index)}>
              {state.copied === `copy:${item.id}:${index}` ? t.copied : t.copy}</button>
          </div>)}
          {item.commands.length > 0 && <p className="field-help">{t.commandsHelp}</p>}
        </li>;
      })}
    </ul>}
    {!state.catalogPending && state.view === null && <button type="button" onClick={setup.reload}>{t.retryCatalog}</button>}
    <div className="button-row">
      <button id="recheck-ocr-resources" type="button" disabled={blocked || state.view === null} onClick={setup.recheck}>{t.recheck}</button>
      <button id="use-ocr-resources" type="button" className="primary" disabled={blocked || proposal === null} onClick={setup.use}>{t.use}</button>
    </div>
    {block && <p className="inline-info">{block}</p>}
    {stale ? <p className="inline-warning">{t.stale}</p>
      : state.applied ? <p className="inline-info">{t.applied}</p>
      : proposal ? <><p className="inline-info">{t.proposal}</p>
        <details className="ocr-manual"><summary>{t.paths}</summary>
          <dl className="fixed-facts"><dt>{messages[locale].ui.environment.modelRoot}</dt><dd>{proposal.model_root}</dd>
            <dt>{messages[locale].ui.environment.runtime}</dt><dd>{proposal.runtime_path}</dd>
            <dt>{messages[locale].ui.environment.libraries}</dt><dd>{proposal.native_library_paths.join('\n')}</dd></dl>
        </details></>
      : !busy && state.view && <p className="field-help">{state.checkedRevision === null ? t.recheckHelp : t.missing}</p>}
    <p className="authority-note">{t.retained}</p>
  </section>;
}

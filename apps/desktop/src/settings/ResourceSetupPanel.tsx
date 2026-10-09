import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {SUPPORTED_PROFILES} from '../state.ts';
import type {OcrSetupControls} from './useOcrSetup.ts';

export default function ResourceSetupPanel({setup, block}:{setup:OcrSetupControls; block:string|null}) {
  const locale = useLocale();
  const t = messages[locale].ui.ocrSetup;
  const {state, busy, stale, draft} = setup;
  const operation = state.operation;
  const progress = operation?.progress;
  const success = operation !== null && !operation.active && progress?.stage === 'complete' && !operation.error && !operation.cleanup_error;
  const downloaded = state.owned && success && state.resourceId !== 'inspect';
  const blocked = block !== null || busy || state.catalogPending;
  const bytes = new Intl.NumberFormat(locale);
  const selected = {
    'rapidocr-models':Boolean(draft.model_root.trim()),
    onnxruntime:Boolean(draft.runtime_path.trim()),
    'native-libraries':Boolean(draft.library_paths.trim()),
  };
  const supported = SUPPORTED_PROFILES.some(item => item.profile === draft.profile);
  const selection = state.nativeSelection;
  const folders = selection?.method === 'folders' ? selection.paths : [];
  const needsAttention = state.view?.items.some(item => item.state === 'missing' || item.state === 'incompatible' || item.state === 'unsupported') || progress?.stage === 'failed';
  const remaining = [
    ...(!selected['rapidocr-models'] ? [t.nextModels] : []),
    ...(!selected.onnxruntime ? [t.nextRuntime] : []),
    ...(!selected['native-libraries'] ? [state.view && state.view.native_methods.length === 0 ? t.nextUnsupported : t.nextNative] : []),
    ...(!supported && draft.profile ? [t.nextProfile] : !supported && selected['rapidocr-models'] && selected.onnxruntime && selected['native-libraries'] ? [t.nextBlankProfile] : []),
    ...(needsAttention ? [t.nextAttention] : []),
  ];
  return <section className="ocr-setup" aria-labelledby="ocr-setup-heading">
    <h4 id="ocr-setup-heading">{t.heading}</h4>
    <p className="muted">{t.introduction}</p>
    {state.catalogPending && <p role="status" className="muted">{t.loading}</p>}
    {(busy || operation) && <div className="ocr-setup-progress" role="status">
      <strong>{state.pickerPending ? t.choosingFolder : state.cancelRequested && busy && operation?.active !== false ? t.cancelling : state.starting ? t.starting : progress ? t.stage(progress.stage) : t.awaitingProgress}</strong>
      {operation?.active && progress && <>
        <progress aria-label={t.progress} value={progress.total > 0 ? Math.min(progress.bytes, progress.total) : undefined} max={progress.total || 1}/>
        {progress.total > 0 && <span className="mono">{bytes.format(progress.bytes)} / {bytes.format(progress.total)} {t.bytes}</span>}
      </>}
      {state.pickerPending ? <button type="button" onClick={setup.cancel}>{t.cancel}</button>
        : busy && state.owned && operation?.active !== false && <button type="button" disabled={state.cancelPending || (state.cancelRequested && state.error === null)} onClick={setup.cancel}>{t.cancel}</button>}
    </div>}
    {operation && !state.owned && <p className="inline-info">{t.previous}</p>}
    {state.error !== null && <details className="ocr-setup-details"><summary>{t.failed}</summary><FaultMessage title={t.failed} value={fault(state.error)}/></details>}
    {state.pollError !== null && <details className="ocr-setup-details"><summary>{t.progressFailed}</summary><FaultMessage title={t.progressFailed} value={fault(state.pollError)}/></details>}
    {state.pollError !== null && state.id === null && <button type="button" disabled={state.catalogPending} onClick={setup.reload}>{t.retryStatus}</button>}
    {state.unavailable && <p className="inline-warning" role="alert">{t.unavailable}</p>}
    {operation?.error && <details className="ocr-setup-details"><summary>{progress?.stage === 'cancelled' ? t.cancelled : t.failed}</summary><FaultMessage title={t.failed} value={operation.error}/></details>}
    {operation?.cleanup_error && <><details className="ocr-setup-details"><summary>{t.cleanupFailed}</summary><FaultMessage title={t.cleanupFailed} value={operation.cleanup_error}/></details><p className="inline-warning">{t.cleanupHelp}</p></>}
    {downloaded && <p className="inline-info">{state.cancelRequested || stale ? t.downloadedRetained : t.downloaded}</p>}
    {state.owned && success && operation?.result === null && <p className="inline-warning" role="alert">{t.noResult}</p>}
    {state.pickerStale && <p className="inline-warning" role="alert">{t.pickerStale}</p>}
    {state.view && <ul className="ocr-setup-items">
      {state.view.items.map(item => {
        const name = t.itemNames[item.id as keyof typeof t.itemNames] ?? t.otherItem;
        const hasPath = selected[item.id as keyof typeof selected] ?? false;
        const owned = state.resourceId === item.id;
        const verifiedDownload = owned && downloaded;
        const attention = item.state === 'missing' || item.state === 'incompatible' || item.state === 'unsupported';
        const itemStatus = owned && busy ? t.stage(progress?.stage ?? 'resolving')
          : attention ? t.state(item.state) : hasPath ? t.selectedState : verifiedDownload ? t.downloadedState : t.state(item.state);
        return <li key={item.id} className="ocr-setup-item">
          <div className="section-heading"><strong>{name}</strong>
            <span className={`tag ${attention ? 'stale' : hasPath || verifiedDownload || item.state === 'verified' ? 'current' : ''}`}>{itemStatus}</span></div>
          <p className="field-help">{t.itemHelp[item.id as keyof typeof t.itemHelp] ?? t.otherHelp}</p>
          <div className="button-row">
            {item.downloadable && <button type="button" disabled={blocked || (hasPath && (verifiedDownload || item.state === 'verified'))} onClick={() => setup.start(item.id)}>{t.download}</button>}
            {item.id === 'native-libraries' && <>
              {state.view?.native_methods.includes('homebrew') && <button type="button" disabled={blocked} aria-pressed={selection?.method === 'homebrew'} onClick={setup.homebrew}>{t.homebrew}</button>}
              {state.view?.native_methods.includes('folders') && <button type="button" disabled={blocked} aria-pressed={selection?.method === 'folders'} onClick={setup.chooseFolder}>{t.chooseFolder}</button>}
              {selection?.method === 'folders' && <button type="button" disabled={blocked || folders.length >= 8} onClick={setup.addFolder}>{t.addFolder}</button>}
              {selection && <button type="button" disabled={blocked} onClick={setup.recheck}>{t.checkSelection}</button>}
            </>}
          </div>
          {item.id === 'native-libraries' && selection && <p className="field-help">{selection.method === 'homebrew' ? t.homebrewSelected : t.foldersSelected}</p>}
          <details className="ocr-setup-details"><summary>{t.details}</summary>
            <p className="field-help">{item.name[locale]} · {item.detail[locale]}</p>
            {item.id === 'native-libraries' && folders.length > 0 && <><strong>{t.selectedFolders}</strong><ul>{folders.map(path => <li key={path}><code>{path}</code></li>)}</ul></>}
            <div className="button-row">{item.links.map((link, index) => <button type="button" className="link" key={index} disabled={state.guidancePending !== null}
              onClick={() => setup.guidance('link', item.id, index)} title={link.url}>{link.label[locale]} ↗</button>)}</div>
            {item.commands.map((command, index) => <div className="ocr-setup-command" key={index}>
              <pre>{command}</pre><button type="button" disabled={state.guidancePending !== null}
                aria-label={`${t.copy}: ${name}`} onClick={() => setup.guidance('copy', item.id, index)}>
                {state.copied === `copy:${item.id}:${index}` ? t.copied : t.copy}</button>
            </div>)}
            {item.commands.length > 0 && <p className="field-help">{t.commandsHelp}</p>}
          </details>
        </li>;
      })}
    </ul>}
    {!state.catalogPending && state.view === null && <button type="button" onClick={setup.reload}>{t.retryCatalog}</button>}
    <div className="button-row"><button id="recheck-ocr-resources" type="button" disabled={blocked || state.view === null} onClick={setup.recheck}>{t.recheck}</button></div>
    {block && <p className="inline-info">{block}</p>}
    {stale && <p className="inline-warning">{t.stale}</p>}
    {state.adopted && !stale && <p className="inline-info">{t.adopted}</p>}
    {remaining.length > 0 ? <div className="ocr-setup-next"><p>{t.remaining}</p><ul>{remaining.map(step => <li key={step}>{step}</li>)}</ul></div>
      : supported && <p className="inline-info">{t.ready}</p>}
    <p className="field-help">{t.recheckHelp}</p>
    {(draft.model_root || draft.runtime_path || draft.library_paths) && <details className="ocr-setup-details"><summary>{t.paths}</summary>
      <dl className="fixed-facts"><dt>{messages[locale].ui.environment.modelRoot}</dt><dd>{draft.model_root || '—'}</dd>
        <dt>{messages[locale].ui.environment.runtime}</dt><dd>{draft.runtime_path || '—'}</dd>
        <dt>{messages[locale].ui.environment.libraries}</dt><dd>{draft.library_paths || '—'}</dd></dl>
    </details>}
    {state.id && <details className="ocr-setup-details"><summary>{t.operationDetails}</summary><code className="ocr-setup-id">{state.id}</code></details>}
    <p className="authority-note">{t.retained}</p>
  </section>;
}

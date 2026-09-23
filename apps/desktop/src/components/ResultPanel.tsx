import {DISCLOSURE_LIMIT, boundedText, cleanupLabel, initializationLabel, record, text} from '../state.ts';
import type {ControllerView, Fault, Json} from '../types.ts';
import {LocalFault, messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export function fault(error: unknown): Fault {
  if (error instanceof LocalFault) return error;
  if (error !== null && typeof error === 'object' && 'message' in error) {
    const value = error as Partial<Fault>;
    return {category: value.category ?? 'Application', message: String(value.message), context: value.context ?? null};
  }
  return {category: 'Application', message: String(error), context: null};
}

export function FaultMessage({value, title}: {value: Fault; title: string}) {
  const locale = useLocale();
  return <section className="fault" role="alert"><strong>{title} · {value.category}</strong><p>{value instanceof LocalFault ? renderMessage(locale, value.presentation) : value.message}</p>
    {value.context !== null && <pre className="diagnostic">{JSON.stringify(value.context, null, 2)}</pre>}
  </section>;
}

export function BoundedRecord({value}: {value: Json}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const bounded = boundedText(JSON.stringify(value, null, 2), DISCLOSURE_LIMIT);
  return <>
    <pre>{bounded.text}</pre>
    {bounded.truncated > 0 && <p className="inline-warning">{t.result.truncated(bounded.truncated)}</p>}
  </>;
}

export default function ResultPanel({view, disclosed, onDisclose}: {view: ControllerView; disclosed: boolean; onDisclose: (next: boolean) => void}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const result = view.result;
  // Preparation faults carry the same operation facts in their context as a settled result.
  const source = result ?? record(view.error?.context);
  const observations = record(result?.observations);
  const build = record(result?.build);
  const check = view.operation === 'environment_check';
  const lane = text(result?.lane) ?? text(source.lane);
  // Replay observations and script decisions can carry recognized text: private until disclosed.
  const privateDetail = !check && lane !== null && lane !== 'controlled';
  const completed = Array.isArray(source.completed_stages) ? source.completed_stages.map(String) : [];
  return <>
    <div className="result-facts">
      <div><span>{t.result.status}</span><strong>{String(result?.status ?? (view.state === 'terminal' && view.error ? view.error.category : t.result.notSettled))}</strong></div>
      <div><span>{t.result.entry}</span><strong>{result?.entry_outcome == null ? t.common.unobserved : t.entryOutcome(String(result.entry_outcome))}</strong></div>
      <div><span>{t.common.cleanup}</span><strong>{cleanupLabel(result, source, locale)}</strong></div>
      <div><span>{t.result.containment}</span><strong>{result?.forced === true ? t.common.yes : result?.forced === false ? t.common.no : t.common.unobserved}</strong></div>
    </div>
    <dl className="run-identity identity-facts" id="identity-facts">
      <dt>{t.common.stage}</dt><dd>{text(source.stage) ?? (view.state === 'idle' ? t.common.noOperation : t.common.unobserved)}</dd>
      <dt>{t.common.completed}</dt><dd>{completed.length ? completed.join(' → ') : t.common.noneRecorded}</dd>
      <dt>{t.common.environment}</dt><dd><code>{text(source.environment_identity) ?? t.common.notDerived}</code></dd>
      <dt>{t.common.corpus}</dt><dd><code>{text(source.corpus_identity) ?? t.common.notDerived}</code></dd>
      {check && <><dt>{t.common.selection}</dt><dd><code>{text(source.selection_identity) ?? t.common.unobserved}</code></dd><dt>{t.common.initialization}</dt><dd id="initialization">{initializationLabel(view.progress, locale)}</dd></>}
      <dt>{t.result.childBuild}</dt><dd>{build.engine_enabled === undefined ? t.result.noBuild : <>{build.engine_enabled === true ? t.result.engineEnabled : t.result.engineAbsent} · <code>{text(build.executable_sha256) ?? t.result.unhashed}</code></>}</dd>
    </dl>
    {result && check && <div className="outcome-details">
      <p className="muted">{t.result.checkHelp}</p>
      <details><summary>{t.result.engine}</summary><BoundedRecord value={observations.engine ?? null}/></details>
      <details><summary>{t.result.cleanupEvidence}</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    {result && !check && <div className="outcome-details">
      {privateDetail && !disclosed
        ? <p className="muted">{t.result.privateHelp}</p>
        : <>
          <h3>{t.result.decisions}</h3>
          {Array.isArray(observations.logs) && observations.logs.length > 0
            ? <ul className="decision-list">{observations.logs.map((item, index) => <li key={index}>{typeof item === 'string' ? item : JSON.stringify(item)}</li>)}</ul>
            : <p className="muted">{t.result.noDecisions}</p>}
          <div className="evidence-grid">{['dispatches', 'receipts', 'accepted', 'effects', 'postconditions', 'sink', ...(privateDetail ? ['engine'] : [])].map(key => <details key={key}>
            <summary>{key}{Array.isArray(observations[key]) ? ` · ${observations[key].length}` : ''}</summary>
            <BoundedRecord value={observations[key] ?? null}/>
          </details>)}</div>
        </>}
      <details><summary>{t.result.cleanupEvidence}</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    <div id="result" className="private-disclosure">
      <button id="disclose-result" disabled={!result && !view.error} onClick={() => onDisclose(!disclosed)}>{disclosed ? t.result.hide : t.result.disclose}</button>
      <span className="muted">{t.result.disclosureHelp(privateDetail, DISCLOSURE_LIMIT / 1024)}</span>
      {disclosed && <BoundedRecord value={result ?? (view.error ? {category: view.error.category, message: view.error.message, context: view.error.context} : null)}/>}
    </div>
  </>;
}

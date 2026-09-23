import {DISCLOSURE_LIMIT, boundedText, cleanupLabel, initializationLabel, record, text} from '../state.ts';
import type {ControllerView, Fault, Json} from '../types.ts';

export function fault(error: unknown): Fault {
  if (error !== null && typeof error === 'object' && 'message' in error) {
    const value = error as Partial<Fault>;
    return {category: value.category ?? 'Application', message: String(value.message), context: value.context ?? null};
  }
  return {category: 'Application', message: String(error), context: null};
}

export function FaultMessage({value, title}: {value: Fault; title: string}) {
  return <section className="fault" role="alert"><strong>{title} · {value.category}</strong><p>{value.message}</p>
    {value.context !== null && <pre className="diagnostic">{JSON.stringify(value.context, null, 2)}</pre>}
  </section>;
}

export function BoundedRecord({value}: {value: Json}) {
  const bounded = boundedText(JSON.stringify(value, null, 2), DISCLOSURE_LIMIT);
  return <>
    <pre>{bounded.text}</pre>
    {bounded.truncated > 0 && <p className="inline-warning">Display truncated: {bounded.truncated} more characters are retained by the backend record but not rendered.</p>}
  </>;
}

export default function ResultPanel({view, disclosed, onDisclose}: {view: ControllerView; disclosed: boolean; onDisclose: (next: boolean) => void}) {
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
      <div><span>Result status</span><strong>{String(result?.status ?? (view.state === 'terminal' && view.error ? view.error.category : 'Not settled'))}</strong></div>
      <div><span>Entry outcome</span><strong>{String(result?.entry_outcome ?? 'Unobserved')}</strong></div>
      <div><span>Cleanup</span><strong>{cleanupLabel(result, source)}</strong></div>
      <div><span>Forced containment</span><strong>{result?.forced === true ? 'Yes' : result?.forced === false ? 'No' : 'Unobserved'}</strong></div>
    </div>
    <dl className="run-identity identity-facts" id="identity-facts">
      <dt>Stage</dt><dd>{text(source.stage) ?? (view.state === 'idle' ? 'No operation yet' : 'Unobserved')}</dd>
      <dt>Completed</dt><dd>{completed.length ? completed.join(' → ') : 'None recorded'}</dd>
      <dt>Environment</dt><dd><code>{text(source.environment_identity) ?? 'Not derived'}</code></dd>
      <dt>Corpus</dt><dd><code>{text(source.corpus_identity) ?? 'Not derived'}</code></dd>
      {check && <><dt>Selection</dt><dd><code>{text(source.selection_identity) ?? 'Unobserved'}</code></dd><dt>Initialization</dt><dd id="initialization">{initializationLabel(view.progress)}</dd></>}
      <dt>Child build</dt><dd>{build.engine_enabled === undefined ? 'No startup identity observed' : <>engine {build.engine_enabled === true ? 'enabled' : 'absent'} · <code>{text(build.executable_sha256) ?? 'unhashed'}</code></>}</dd>
    </dl>
    {result && check && <div className="outcome-details">
      <p className="muted">No package module, readiness, or workflow code was evaluated. Engine facts are the child's own report.</p>
      <details><summary>Engine facts</summary><BoundedRecord value={observations.engine ?? null}/></details>
      <details><summary>Cleanup evidence</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    {result && !check && <div className="outcome-details">
      {privateDetail && !disclosed
        ? <p className="muted">Script decisions and observation records stay hidden until disclosed below. Full records are not automatically copied to ordinary logs; scripts can explicitly emit bounded messages.</p>
        : <>
          <h3>Script decisions</h3>
          {Array.isArray(observations.logs) && observations.logs.length > 0
            ? <ul className="decision-list">{observations.logs.map((item, index) => <li key={index}>{typeof item === 'string' ? item : JSON.stringify(item)}</li>)}</ul>
            : <p className="muted">No decision was recorded.</p>}
          <div className="evidence-grid">{['dispatches', 'receipts', 'accepted', 'effects', 'postconditions', 'sink', ...(privateDetail ? ['engine'] : [])].map(key => <details key={key}>
            <summary>{key}{Array.isArray(observations[key]) ? ` · ${observations[key].length}` : ''}</summary>
            <BoundedRecord value={observations[key] ?? null}/>
          </details>)}</div>
        </>}
      <details><summary>Cleanup evidence</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    <div id="result" className="private-disclosure">
      <button id="disclose-result" disabled={!result && !view.error} onClick={() => onDisclose(!disclosed)}>{disclosed ? 'Hide full record' : 'Disclose full record · private'}</button>
      <span className="muted">Build metadata, diagnostics{privateDetail ? ', and recognition detail' : ''}. Rendered here only, bounded to {DISCLOSURE_LIMIT / 1024} KiB.</span>
      {disclosed && <BoundedRecord value={result ?? (view.error ? {category: view.error.category, message: view.error.message, context: view.error.context} : null)}/>}
    </div>
  </>;
}

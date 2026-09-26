import {FaultMessage} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {
  MAX_DEFINITIONS, MAX_EXPECTED_BYTES, UNDO_ENTRIES, confirmBlock, copyBlock, copyFreshness, definitionIssue, deleteDefinition, geometryConfirmed,
  mapRegion, recognitionDirty, renameDefinition, sameJson, saveBlock, selectDefinition, setExpected, setKind, setRights, toggleCrop, toggleTrial,
  trialBlock, trialEnvelope, trialFreshness, undoRecognition, utf8Bytes,
} from '../recognition.ts';
import type {NormalizedRect, RecognitionDefinition, RecognitionKind, RecognitionState, SnippetKind, TemplateRights, TrialDiagnostics} from '../recognition.ts';

export interface RecognitionHandlers {
  // Explicit PNG selection, then `recognition_load`.
  load: () => void;
  confirm: () => void;
  // Grouped OCR or one template against the loaded frame, or one saved OCR crop sample.
  trial: (kind: 'frame' | 'sample', ids: string[]) => void;
  // The independent authoring Stop.
  stop: () => void;
  save: () => void;
  copy: (id: string, kind: SnippetKind) => void;
  discard: () => void;
  capabilities: () => void;
  openPreview: () => void;
  reload: () => void;
}

export interface RecognitionPageProps {
  state: RecognitionState;
  // Local metadata edits (pure reducers from recognition.ts) applied to the Edit session's state.
  onState: (update: (state: RecognitionState) => RecognitionState) => void;
  handlers: RecognitionHandlers;
  // Command admission only: another host command is in flight or the application is closing. Local metadata
  // edits, selection and Undo stay available.
  locked: boolean; lockReason: string | null;
  // The host no longer reports this lease: nothing can be sent or edited; the draft is kept for Exit.
  leaseLost: boolean;
  // The recognition child holds the work reservation, as the host controller reports; Stop stays reachable.
  trialActive: boolean;
}

const EMPTY_RIGHTS: TemplateRights = {license: '', created_by: '', created_for: null, reviewed: false};
const MIB = 1_048_576;

// Main-window recognition panel: frame and geometry status, definitions, trials, crop-only Save and one-way Copy.
// On-image editing happens in the detached preview window, which shares this state through the Edit session.
export default function RecognitionPage({state, onState, handlers, locked, lockReason, leaseLost, trialActive}: RecognitionPageProps) {
  const locale = useLocale();
  const r = messages[locale].ui.recognition;
  const {document, view} = state;
  const frame = view.frame;
  const basis = document?.basis ?? null;
  const definitions = document?.definitions ?? [];
  const selected = definitions.find(item => item.id === state.selected) ?? null;
  const confirmed = geometryConfirmed(state);
  const running = trialActive || state.running !== null;
  const commands = locked || leaseLost;
  const limit = view.capabilities.max_ocr_zones;
  const policy = view.capabilities.image_policy;
  const number = (value: number) => value.toLocaleString(locale, {maximumFractionDigits: 4});

  function pixels(region: NormalizedRect): string {
    const edges = basis && mapRegion(region, basis);
    return edges ? r.rect(edges.left, edges.top, edges.right - edges.left, edges.bottom - edges.top) : r.issue('region');
  }

  function kindLabel(kind: RecognitionKind): string {
    return kind === 'ocr' ? r.kindOcr : r.kindTemplate;
  }

  function nameOf(id: string): string {
    return definitions.find(item => item.id === id)?.name ?? id;
  }

  const record = state.trial;
  const envelope = record?.trial ? trialEnvelope(record.trial) : null;
  const result: TrialDiagnostics | null = envelope?.result ?? null;
  const trialIds = new Set(result?.kind === 'ocr' ? result.zones.map(zone => zone.id) : result ? [result.id] : []);
  function freshnessTag(id: string) {
    const freshness = trialFreshness(state, id);
    return <span className={`tag ${freshness === 'fresh' ? 'current' : 'stale'}`} title={r.freshnessHelp(freshness)}>{r.freshness(freshness)}</span>;
  }

  const confirmReason = confirmBlock(state);
  const ocrBlock = trialBlock(state, 'frame', state.trialIds);
  const templateBlock = selected?.kind === 'template' ? trialBlock(state, 'frame', [selected.id]) : null;
  const sampleBlock = selected?.kind === 'ocr' && selected.saved ? trialBlock(state, 'sample', [selected.id]) : null;
  const saveReason = saveBlock(state);
  const dirty = recognitionDirty(state);
  const rights = document?.template_rights ?? null;
  const showRights = rights !== null || definitions.some(item => item.kind === 'template');
  const savedDefinition = selected ? view.saved_document?.definitions.find(item => item.id === selected.id) ?? null : null;

  function editRights(change: Partial<TemplateRights>) {
    onState(current => setRights(current, {...(current.document?.template_rights ?? EMPTY_RIGHTS), ...change}));
  }

  function copyButton(definition: RecognitionDefinition, kind: SnippetKind, label: string) {
    const reason = copyBlock(state, definition.id, kind);
    return <button id={`recognition-copy-${kind}`} type="button" disabled={commands || reason !== null} title={reason ? r.block(reason) : undefined}
      onClick={() => handlers.copy(definition.id, kind)}>{label}</button>;
  }

  const copy = selected ? state.copies[selected.id] : undefined;
  const copyState = selected ? copyFreshness(state, selected.id) : null;

  return <section id="recognition" className="recognition-page" aria-labelledby="recognition-heading">
    <div className="recognition-header">
      <div><h3 id="recognition-heading">{r.heading}</h3><p className="field-help">{r.intro}</p></div>
      <div className="button-row">
        <button id="recognition-load" type="button" disabled={commands || running} onClick={handlers.load}>{frame ? r.replaceImage : r.loadImage}</button>
        <button id="recognition-open-preview" type="button" disabled={leaseLost} title={r.previewHelp} onClick={handlers.openPreview}>{r.openPreview}</button>
        <button id="recognition-reload" type="button" disabled={commands} onClick={handlers.reload}>{r.reload}</button>
      </div>
    </div>
    {lockReason && <p className="muted" role="status">{lockReason}</p>}
    {state.notice && <p id="recognition-notice" className="inline-warning" role="status">{r.notice(state.notice)}</p>}
    {state.error && <FaultMessage title={r.actionFailed} value={state.error}/>}

    <div className="repo-facts recognition-facts">
      <dl>
        <div><dt>{r.frameHeading}</dt><dd id="recognition-frame" className="mono">{frame ? r.frame(frame.width, frame.height) : r.noFrameShort}</dd></div>
        <div><dt>{r.contentHeading}</dt><dd id="recognition-content" className="mono">
          {basis ? r.rect(basis.content.x, basis.content.y, basis.content.width, basis.content.height) : r.none}</dd></div>
        <div><dt>{r.geometry}</dt><dd>
          <span id="recognition-geometry" className={`tag ${confirmed ? 'current' : 'stale'}`}>{confirmed ? r.confirmed : r.unconfirmed}</span>
          {frame && <button id="recognition-confirm" type="button" disabled={commands || confirmReason !== null}
            title={confirmReason ? r.block(confirmReason) : undefined} onClick={handlers.confirm}>{r.confirm}</button>}
        </dd></div>
      </dl>
    </div>
    <p className="field-help">{frame ? confirmed ? r.previewHelp : r.confirmHelp : document ? r.noFrameSaved : r.noFrame}</p>
    <p className="field-help">{r.limits(Math.round(policy.input_bytes / MIB), policy.input_pixels.toLocaleString(locale), Math.round(policy.crop_bytes / MIB),
      policy.crop_pixels.toLocaleString(locale))}</p>

    <section className="panel recognition-panel" aria-labelledby="recognition-definitions-heading">
      <div className="panel-heading"><h4 id="recognition-definitions-heading">{r.definitionsHeading}</h4>
        <span className="muted">{r.count(definitions.length, MAX_DEFINITIONS)}</span>
        <button id="recognition-undo" type="button" disabled={leaseLost || state.undo.length === 0} title={r.undoHelp(state.undo.length, UNDO_ENTRIES)}
          onClick={() => onState(undoRecognition)}>{r.undo}</button></div>
      <div className="panel-body">
        {definitions.length === 0 ? <p className="muted">{frame ? r.noDefinitions : r.noDefinitionsFrame}</p>
          : <table id="recognition-definitions" className="recognition-list">
            <thead><tr><th scope="col">{r.columnTrial}</th><th scope="col">{r.columnName}</th><th scope="col">{r.columnKind}</th>
              <th scope="col">{r.columnRegion}</th><th scope="col">{r.columnState}</th><th scope="col">{r.columnCrop}</th></tr></thead>
            <tbody>{definitions.map(definition => {
              const current = definition.id === state.selected;
              const issue = basis && definitionIssue(definition, basis);
              return <tr key={definition.id} data-id={definition.id} className={current ? 'selected' : undefined}>
                <td>{definition.kind === 'ocr'
                  ? <input type="checkbox" checked={state.trialIds.includes(definition.id)} aria-label={r.trialFor(definition.name)}
                    onChange={() => onState(current => toggleTrial(current, definition.id))}/>
                  : <span className="muted" aria-hidden="true">—</span>}</td>
                <td><button type="button" className="link recognition-name" aria-current={current ? 'true' : undefined}
                  onClick={() => onState(current => selectDefinition(current, definition.id))}>{definition.name || r.unnamed}</button></td>
                <td><span className="tag">{kindLabel(definition.kind)}</span></td>
                <td className="mono">{pixels(definition.region)}</td>
                <td><span className="recognition-tags">
                  {issue && <span className="tag unsaved">{r.issue(issue)}</span>}
                  {definition.saved && <span className="tag current">{r.savedCrop}</span>}
                  {trialIds.has(definition.id) && freshnessTag(definition.id)}
                </span></td>
                <td><input type="checkbox" checked={state.cropIds.includes(definition.id)} disabled={leaseLost || frame === null}
                  aria-label={r.cropFor(definition.name)} onChange={() => onState(current => toggleCrop(current, definition.id))}/></td>
              </tr>;
            })}</tbody>
          </table>}
      </div>
    </section>

    <section className="panel recognition-panel" aria-labelledby="recognition-selected-heading">
      <div className="panel-heading"><h4 id="recognition-selected-heading">{r.selectedHeading}</h4>
        {selected && <button id="recognition-delete" type="button" className="danger-text" disabled={leaseLost}
          onClick={() => onState(current => deleteDefinition(current, selected.id))}>{r.delete}</button>}</div>
      <div className="panel-body">
        {!selected ? <p className="muted">{r.noSelection}</p> : <>
          <div className="field"><label htmlFor="recognition-name">{r.name}</label>
            <input id="recognition-name" type="text" value={selected.name} disabled={leaseLost} spellCheck={false}
              onChange={event => onState(current => renameDefinition(current, selected.id, event.target.value))}/>
            {!selected.name.trim() && <p className="field-help inline-warning">{r.issue('name')}</p>}</div>
          <div className="field"><label htmlFor="recognition-kind">{r.kind}</label>
            <select id="recognition-kind" value={selected.kind} disabled={leaseLost}
              onChange={event => onState(current => setKind(current, selected.id, event.target.value as RecognitionKind))}>
              <option value="ocr">{r.kindOcr}</option><option value="template">{r.kindTemplate}</option></select>
            <p className="field-help">{r.kindHelp}</p></div>
          <dl className="recognition-geometry">
            <div><dt>{selected.kind === 'ocr' ? r.region : r.pattern}</dt><dd className="mono">{pixels(selected.region)}</dd></div>
            {selected.template && <div><dt>{r.search}</dt><dd className="mono">{pixels(selected.template.search_region)}</dd></div>}
            {selected.template && <div><dt>{r.matchDefaults}</dt><dd>{r.threshold(number(selected.template.threshold), selected.template.max_results)}</dd></div>}
            {selected.saved && <div><dt>{r.savedCrop}</dt><dd className="mono" title={selected.saved.sha256}>
              {r.savedFacts(selected.saved.asset, selected.saved.width, selected.saved.height)}</dd></div>}
          </dl>
          {basis && definitionIssue(selected, basis) && <p className="inline-warning">{r.issue(definitionIssue(selected, basis)!)}</p>}
          {selected.kind === 'template' && !selected.saved && <p className="field-help">{r.templateNeedsCrop}</p>}
          {selected.kind === 'template' && selected.saved && savedDefinition && !sameJson(savedDefinition.region, selected.region)
            && <p className="inline-warning">{r.patternChanged}</p>}
          {selected.kind === 'ocr' && <div className="field"><label htmlFor="recognition-wait-text">{r.waitText}</label>
            <textarea id="recognition-wait-text" rows={2} value={selected.expected ?? ''} disabled={leaseLost} spellCheck={false}
              aria-describedby="recognition-wait-text-help" onChange={event => onState(current => setExpected(current, selected.id, event.target.value))}/>
            <p id="recognition-wait-text-help" className="field-help">{r.waitTextHelp} {r.bytes(utf8Bytes(selected.expected ?? ''), MAX_EXPECTED_BYTES)}</p></div>}
        </>}
      </div>
    </section>

    <section className="panel recognition-panel" aria-labelledby="recognition-trial-heading">
      <div className="panel-heading"><h4 id="recognition-trial-heading">{r.trialHeading}</h4>
        <span id="recognition-engine-limit" className="muted">{limit === null ? r.engineUnknown : r.engineLimit(limit)}</span></div>
      <div className="panel-body">
        <p className="field-help">{r.trialHelp}</p>
        {limit === null && <div className="button-row"><span className="inline-warning">{r.capabilityMissing}</span>
          <button id="recognition-capabilities" type="button" disabled={commands || running} onClick={handlers.capabilities}>{r.capabilities}</button></div>}
        <div className="button-row">
          <button id="recognition-try-ocr" type="button" className="primary" disabled={commands || running || ocrBlock !== null}
            title={ocrBlock ? r.block(ocrBlock) : undefined} onClick={() => handlers.trial('frame', state.trialIds)}>{r.tryOcr}</button>
          <span id="recognition-trial-selection" className={ocrBlock === 'overLimit' ? 'inline-warning' : 'muted'}>{r.selection(state.trialIds.length, limit)}</span>
          {selected?.kind === 'template' && <button id="recognition-try-template" type="button" disabled={commands || running || templateBlock !== null}
            title={templateBlock ? r.block(templateBlock) : undefined} onClick={() => handlers.trial('frame', [selected.id])}>{r.tryTemplate}</button>}
          {selected?.kind === 'ocr' && selected.saved && <button id="recognition-recheck-sample" type="button" disabled={commands || running || sampleBlock !== null}
            title={sampleBlock ? r.block(sampleBlock) : r.sampleHelp} onClick={() => handlers.trial('sample', [selected.id])}>{r.recheckSample}</button>}
          {running && <button id="recognition-stop" type="button" className="stop-button" onClick={handlers.stop}>{r.stop}</button>}
        </div>
        {ocrBlock && ocrBlock !== 'empty' && <p className="muted">{r.block(ocrBlock)}</p>}
        {running && <p className="muted" role="status">{r.trialRunning}</p>}
        {!record && !running && <p className="muted">{r.noTrial}</p>}
        {record?.fault && <FaultMessage title={r.trialRefused} value={record.fault}/>}
        {record?.trial && <div id="recognition-results" className="recognition-results">
          {record.trial.sample_id && <p><span className="tag">{r.sampleTrial}</span> <span className="field-help">{r.sampleHelp}</span></p>}
          {record.trial.controller.error && <FaultMessage title={r.primaryFailed} value={record.trial.controller.error}/>}
          {envelope?.primary && <FaultMessage title={r.primaryFailed} value={envelope.primary}/>}
          {result?.kind === 'ocr' && <>
            <p className="field-help">{r.textContract}</p>
            {result.zones.map(zone => <article key={zone.id} className="recognition-result" data-id={zone.id}>
              <header><strong>{nameOf(zone.id)}</strong> {freshnessTag(zone.id)}
                <span className="tag">{zone.outcome === 'recognized' ? r.recognized : r.noMatch}</span>
                <span className="muted">{r.regions(zone.regions.length)}</span></header>
              {zone.regions.length > 0 && <ol className="recognition-regions">{zone.regions.map((region, index) => <li key={index}>
                <pre className="recognition-text">{region.text}</pre>
                <span className="muted">{r.confidence}: {region.confidence === null ? r.confidenceUnavailable : number(region.confidence)}</span>
                <span className="muted mono">{r.bounds}: {r.rect(region.bounds.x, region.bounds.y, region.bounds.width, region.bounds.height)}</span>
              </li>)}</ol>}
            </article>)}
          </>}
          {result?.kind === 'template' && <article className="recognition-result" data-id={result.id}>
            <header><strong>{nameOf(result.id)}</strong> {freshnessTag(result.id)}
              <span className="tag">{result.outcome === 'matched' ? r.matched : r.noMatch}</span>
              <span className="muted">{r.templateThreshold(number(result.threshold))}</span></header>
            {result.matches.length === 0 ? <p className="muted">{r.scoreUnavailable}</p>
              : <ol className="recognition-regions">{result.matches.map((match, index) => <li key={index}>
                <span>{r.score(number(match.score))}</span>
                <span className="muted mono">{r.bounds}: {r.rect(match.bounds.x, match.bounds.y, match.bounds.width, match.bounds.height)}</span>
              </li>)}</ol>}
          </article>}
          {envelope && <details className="recognition-cleanup">
            <summary>{r.cleanup}: {r.cleanupState(envelope.child_reaped, envelope.forced)}</summary>
            <pre className="diagnostic">{JSON.stringify(envelope.cleanup, null, 2)}</pre>
          </details>}
        </div>}
      </div>
    </section>

    <section className="panel recognition-panel" aria-labelledby="recognition-save-heading">
      <div className="panel-heading"><h4 id="recognition-save-heading">{r.saveHeading}</h4>
        {dirty && <span className="tag unsaved">{r.unsaved}</span>}</div>
      <div className="panel-body">
        <p className="field-help">{r.saveHelp}</p>
        {showRights && <fieldset className="recognition-rights">
          <legend>{r.rightsHeading}</legend>
          <p className="field-help">{r.rightsHelp}</p>
          <div className="field"><label htmlFor="recognition-license">{r.license}</label>
            <input id="recognition-license" type="text" value={rights?.license ?? ''} disabled={leaseLost} spellCheck={false}
              onChange={event => editRights({license: event.target.value})}/></div>
          <div className="field"><label htmlFor="recognition-created-by">{r.createdBy}</label>
            <input id="recognition-created-by" type="text" value={rights?.created_by ?? ''} disabled={leaseLost} spellCheck={false}
              onChange={event => editRights({created_by: event.target.value})}/></div>
          <div className="field"><label htmlFor="recognition-created-for">{r.createdFor}</label>
            <input id="recognition-created-for" type="text" value={rights?.created_for ?? ''} disabled={leaseLost} spellCheck={false}
              onChange={event => editRights({created_for: event.target.value === '' ? null : event.target.value})}/></div>
          <label className="checkbox-label"><input id="recognition-reviewed" type="checkbox" checked={rights?.reviewed ?? false} disabled={leaseLost}
            onChange={event => editRights({reviewed: event.target.checked})}/> {r.reviewed}</label>
        </fieldset>}
        <div className="button-row">
          <button id="recognition-save" type="button" className="primary" disabled={commands || saveReason !== null}
            title={saveReason ? r.block(saveReason) : undefined} onClick={handlers.save}>{r.save}</button>
          <button id="recognition-discard" type="button" className="danger-text" disabled={commands || !dirty} onClick={handlers.discard}>{r.discard}</button>
          <span className="muted">{r.crops(state.cropIds.length)}</span>
        </div>
        {saveReason && saveReason !== 'noChanges' && <p className="muted">{r.block(saveReason)}</p>}
      </div>
    </section>

    <section className="panel recognition-panel" aria-labelledby="recognition-copy-heading">
      <div className="panel-heading"><h4 id="recognition-copy-heading">{r.copyHeading}</h4></div>
      <div className="panel-body">
        <p className="field-help">{r.copyHelp}</p>
        {!selected ? <p className="muted">{r.noSelection}</p> : <>
          <div className="button-row">
            {selected.kind === 'ocr' && copyButton(selected, 'ocr_recognize', r.copyOcr)}
            {selected.kind === 'ocr' && copyButton(selected, 'ocr_wait', r.copyWait)}
            {selected.kind === 'template' && copyButton(selected, 'template_recognize', r.copyTemplate)}
          </div>
          {selected.kind === 'template' && copyBlock(state, selected.id, 'template_recognize') === 'templateUnsaved' && <p className="muted">{r.block('templateUnsaved')}</p>}
          <p className="field-help">{trialIds.has(selected.id) ? <>{freshnessTag(selected.id)} {r.freshnessHelp(trialFreshness(state, selected.id))}</> : r.noTrialFor}</p>
          {copy && copyState && <div id="recognition-copy-state">
            <span className={`tag ${copyState === 'current' ? 'current' : copyState === 'failed' ? 'unsaved' : 'stale'}`}>
              {copyState === 'current' ? r.copied : copyState === 'failed' ? r.copyFailed : r.copyObsolete}</span>
            {copyState === 'obsolete' && <p className="inline-warning">{r.copyObsoleteHelp}</p>}
            {copy.error && <FaultMessage title={r.copyFailed} value={copy.error}/>}
            {!copy.error && <p className="field-help">{copy.verified ? r.copyVerified : r.copyUnverified}</p>}
            {copy.basis && <p className="field-help mono">{r.basis(copy.basis.frame_width, copy.basis.frame_height, copy.basis.content.x, copy.basis.content.y,
              copy.basis.content.width, copy.basis.content.height)}</p>}
          </div>}
        </>}
      </div>
    </section>
  </section>;
}

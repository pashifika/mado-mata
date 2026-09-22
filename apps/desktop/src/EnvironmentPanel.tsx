import {useState} from 'react';
import {DISCLOSURE_LIMIT, ENVIRONMENT_LANGUAGE, ENVIRONMENT_PROVIDER, ENVIRONMENT_RUNTIME_PROFILE, SUPPORTED_PROFILES, boundedText, cleanupLabel, faultSummary, initializationLabel, record, text} from './state.ts';
import type {CheckAssociation, EnvironmentDraft} from './state.ts';
import type {ControllerView, OcrEnvironment} from './types.ts';

export interface LastCheck {association: CheckAssociation; view: ControllerView}

interface Props {
  draft: EnvironmentDraft; errors: Record<string, string>; onDraft: (next: EnvironmentDraft) => void;
  saved: OcrEnvironment | null; loaded: boolean; dirty: boolean; locked: boolean; active: boolean;
  descriptorPath: string; onDescriptorPath: (path: string) => void;
  onSave: () => void; onCheck: () => void;
  lastCheck: LastCheck | null; stale: string[];
}

function CheckCard({check, stale}: {check: LastCheck; stale: string[]}) {
  const {view, association} = check;
  const [disclosed, setDisclosed] = useState(false);
  const result = view.result;
  const source = result ?? record(view.error?.context);
  const completed = Array.isArray(source.completed_stages) ? source.completed_stages.map(String) : [];
  const failure = view.error ?? record(result?.primary);
  const hasFailure = typeof failure.category === 'string';
  const diagnostic = disclosed ? boundedText(JSON.stringify(failure, null, 2), DISCLOSURE_LIMIT) : null;
  return <div className="check-card" id="last-check">
    <div className="panel-heading"><strong>Last check</strong>
      <span className={`tag ${stale.length ? 'stale' : 'current'}`}>{stale.length ? 'Stale association' : 'Current association'}</span></div>
    <dl className="run-identity">
      <dt>Operation</dt><dd><code>{association.operation}</code></dd>
      <dt>Outcome</dt><dd id="check-outcome">{result ? String(result.status ?? 'Settled') : view.error ? 'Refused' : 'Unsettled'}{hasFailure && <> · {faultSummary(failure, true)}</>}</dd>
      <dt>Stage</dt><dd>{text(source.stage) ?? 'Unobserved'}</dd>
      <dt>Completed</dt><dd>{completed.length ? completed.join(' → ') : 'None recorded'}</dd>
      <dt>Initialization</dt><dd>{initializationLabel(view.progress)}</dd>
      <dt>Selection</dt><dd><code>{text(source.selection_identity) ?? 'Unobserved'}</code></dd>
      <dt>Environment</dt><dd><code>{text(source.environment_identity) ?? 'Not derived'}</code></dd>
      <dt>Corpus</dt><dd><code>{text(source.corpus_identity) ?? 'Not derived'}</code></dd>
      <dt>Descriptor</dt><dd>{association.descriptorPath ?? 'None selected · initialization could not be checked'}</dd>
      <dt>Package</dt><dd><code>{association.packageInventoryIdentity ?? 'None inspected'}</code></dd>
      <dt>Cleanup</dt><dd>{cleanupLabel(result, source)}</dd>
    </dl>
    {hasFailure && <div className="private-disclosure">
      <button onClick={() => setDisclosed(!disclosed)}>{disclosed ? 'Hide check diagnostic' : 'Disclose check diagnostic · private'}</button>
      {diagnostic && <><pre>{diagnostic.text}</pre>{diagnostic.truncated > 0 && <p className="inline-warning">Display truncated: {diagnostic.truncated} more characters are not rendered.</p>}</>}
    </div>}
    {stale.length > 0 && <p className="inline-warning">This result belongs to its original selection only: {stale.join('; ')}. It does not make the current environment appear checked.</p>}
    <p className="authority-note">A check never admits a run. Start captures and revalidates every file again, including same-path replacements; no path is watched.</p>
  </div>;
}

export default function EnvironmentPanel(props: Props) {
  const {draft, errors, onDraft, saved, loaded, dirty, locked, active, descriptorPath, onDescriptorPath, onSave, onCheck, lastCheck, stale} = props;
  const supported = SUPPORTED_PROFILES.find(item => item.profile === draft.profile);
  const savedModel = saved ? SUPPORTED_PROFILES.find(item => item.profile === saved.profile)?.model : undefined;
  const fixedMismatch = saved !== null && (saved.language !== ENVIRONMENT_LANGUAGE || saved.provider !== ENVIRONMENT_PROVIDER || saved.runtime_profile !== ENVIRONMENT_RUNTIME_PROFILE || saved.model !== savedModel);
  const checkBlock = !loaded ? 'Settings are not loaded.' : !saved ? 'Save an OCR environment to enable Check.'
    : dirty ? 'Check reads saved settings only. Save the environment first.' : active ? 'Another operation owns the runner until its terminal outcome is recorded.' : null;
  const blank = !draft.profile && !draft.model_root && !draft.runtime_path && !draft.library_paths;
  function field(key: keyof EnvironmentDraft, value: string) {
    onDraft({...draft, [key]: value});
  }
  return <section className="panel environment-panel" aria-labelledby="environment-heading">
    <div className="panel-heading"><div><span className="eyebrow">APP ENVIRONMENT · MACHINE-LOCAL</span><h2 id="environment-heading">OCR environment</h2></div>
      <span className={`tag ${dirty ? 'unsaved' : ''}`}>{!loaded ? 'Settings not loaded' : dirty ? 'Unsaved changes' : saved ? 'Saved' : 'Unconfigured'}</span></div>
    <div className="environment-grid">
      <div>
        <p className="muted">Saved in App settings, never in portable profiles. Save validates structure only; Check initializes the engine.</p>
        <label htmlFor="ocr-profile">Supported OCR profile</label>
        <select id="ocr-profile" value={draft.profile} disabled={locked} aria-invalid={Boolean(errors.profile)} onChange={event => field('profile', event.target.value)}>
          <option value="">Not configured</option>
          {SUPPORTED_PROFILES.map(item => <option key={item.profile} value={item.profile}>{item.label}</option>)}
          {draft.profile && !supported && <option value={draft.profile} disabled>Unsupported · {draft.profile}</option>}
        </select>
        {errors.profile && <p className="field-error">{errors.profile}</p>}
        <dl className="fixed-facts">
          <dt>Model</dt><dd>{supported?.model ?? '—'}</dd>
          <dt>Language</dt><dd>{ENVIRONMENT_LANGUAGE}</dd>
          <dt>Provider</dt><dd>{ENVIRONMENT_PROVIDER}</dd>
          <dt>Runtime profile</dt><dd>{ENVIRONMENT_RUNTIME_PROFILE}</dd>
        </dl>
        {saved && fixedMismatch && <p className="inline-warning">The saved environment carries a tuple this desktop build does not offer ({saved.model} / {saved.language} / {saved.provider} / {saved.runtime_profile}). Saving replaces it with the supported tuple shown above.</p>}
        <label htmlFor="model-root">Model root directory</label>
        <input id="model-root" type="text" value={draft.model_root} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.model_root)}
          placeholder="Absolute path to the accepted model files" onChange={event => field('model_root', event.target.value)}/>
        {errors.model_root && <p className="field-error">{errors.model_root}</p>}
        <label htmlFor="runtime-path">OCR runtime library</label>
        <input id="runtime-path" type="text" value={draft.runtime_path} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.runtime_path)}
          placeholder="Absolute path to the pinned ONNX runtime library" onChange={event => field('runtime_path', event.target.value)}/>
        {errors.runtime_path && <p className="field-error">{errors.runtime_path}</p>}
        <label htmlFor="library-paths">Reviewed native library paths · one per line</label>
        <textarea id="library-paths" className="library-paths" value={draft.library_paths} disabled={locked} spellCheck={false} rows={3}
          placeholder="Explicit non-system libraries the engine child may load" onChange={event => field('library_paths', event.target.value)}/>
        <p className="muted">Enter absolute paths; Rust canonicalizes them and derives file and SDK identities during Check and again on every Start. Listing a path is not an audit of every loaded image.</p>
        <div className="button-row">
          <button id="save-environment" className="primary" disabled={locked || !loaded || !dirty || Object.keys(errors).length > 0} onClick={onSave}>{blank ? 'Save as unconfigured' : 'Save environment'}</button>
          <button id="clear-environment" disabled={locked || blank} onClick={() => onDraft({profile: '', model_root: '', runtime_path: '', library_paths: ''})}>Clear draft</button>
        </div>
      </div>
      <div>
        <label htmlFor="descriptor-path">Recorded corpus descriptor</label>
        <input id="descriptor-path" type="text" value={descriptorPath} disabled={locked} spellCheck={false}
          placeholder="Absolute path to a replay descriptor JSON" onChange={event => onDescriptorPath(event.target.value)}/>
        <p className="muted">A location hint for this session only. Check and replay Start read the descriptor and its declared assets through the inspected package's inventory each time; unknown or native fields are refused.</p>
        <div className="run-buttons"><button id="check-environment" className="primary" disabled={checkBlock !== null || locked} onClick={onCheck}>Check saved environment</button></div>
        <p className="muted" id="check-block">{checkBlock ?? (descriptorPath.trim() ? 'Validates files, then initializes the engine against the selected corpus without evaluating package code. Stop applies.' : 'Without a descriptor, Check validates files and reports the missing corpus; it cannot claim initialization.')}</p>
        {lastCheck ? <CheckCard key={lastCheck.association.operation} check={lastCheck} stale={stale}/> : <p className="muted">No check has completed in this session.</p>}
      </div>
    </div>
  </section>;
}

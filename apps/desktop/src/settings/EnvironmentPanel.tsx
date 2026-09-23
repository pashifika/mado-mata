import {useState} from 'react';
import Select from '../components/Select';
import {DISCLOSURE_LIMIT, ENVIRONMENT_LANGUAGE, ENVIRONMENT_PROVIDER, ENVIRONMENT_RUNTIME_PROFILE, SUPPORTED_PROFILES, boundedText, cleanupLabel, faultSummary, initializationLabel, record, text} from '../state.ts';
import type {CheckAssociation, EnvironmentDraft} from '../state.ts';
import type {ControllerView, OcrEnvironment, WorkspaceRef} from '../types.ts';

export interface LastCheck {association: CheckAssociation; view: ControllerView}

// What a new Check would bind: the selected workspace's identity and corpus, or nothing.
export interface CheckTarget {workspace: WorkspaceRef | null; label: string; descriptorPath: string | null; packageInventoryIdentity: string | null}

interface Props {
  draft: EnvironmentDraft; errors: Record<string, string>; onDraft: (next: EnvironmentDraft) => void;
  saved: OcrEnvironment | null; loaded: boolean; dirty: boolean; locked: boolean; active: boolean;
  target: CheckTarget; onCheck: () => void;
  lastCheck: LastCheck | null; stale: string[];
  originLabel: (workspaceId: string | null) => string;
}

function CheckCard({check, stale, originLabel}: {check: LastCheck; stale: string[]; originLabel: Props['originLabel']}) {
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
      <dt>Outcome</dt><dd id="check-outcome">{result ? String(result.status ?? 'Settled') : view.error ? faultSummary(view.error, true) : 'Unsettled'}{result && hasFailure && <> · {faultSummary(failure, true)}</>}</dd>
      <dt>Stage</dt><dd>{text(source.stage) ?? 'Unobserved'}</dd>
      <dt>Completed</dt><dd>{completed.length ? completed.join(' → ') : 'None recorded'}</dd>
      <dt>Initialization</dt><dd>{initializationLabel(view.progress)}</dd>
      <dt>Selection</dt><dd><code>{text(source.selection_identity) ?? 'Unobserved'}</code></dd>
      <dt>Environment</dt><dd><code>{text(source.environment_identity) ?? 'Not derived'}</code></dd>
      <dt>Corpus</dt><dd><code>{text(source.corpus_identity) ?? 'Not derived'}</code></dd>
      <dt>Workspace</dt><dd>{association.workspace ? <>{originLabel(association.workspace.workspace_id)} · revision {association.workspace.revision}</> : 'None · validation only'}</dd>
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
  const {draft, errors, onDraft, saved, loaded, dirty, locked, active, target, onCheck, lastCheck, stale, originLabel} = props;
  const supported = SUPPORTED_PROFILES.find(item => item.profile === draft.profile);
  const savedModel = saved ? SUPPORTED_PROFILES.find(item => item.profile === saved.profile)?.model : undefined;
  const fixedMismatch = saved !== null && (saved.language !== ENVIRONMENT_LANGUAGE || saved.provider !== ENVIRONMENT_PROVIDER || saved.runtime_profile !== ENVIRONMENT_RUNTIME_PROFILE || saved.model !== savedModel);
  const checkBlock = !loaded ? 'Settings are not loaded.' : !saved ? 'Save an OCR environment to enable Check.'
    : dirty ? 'Check reads saved settings only. Save changes first.' : active ? 'Another operation owns the runner until its terminal outcome is recorded.' : null;
  const blank = !draft.profile && !draft.model_root.trim() && !draft.runtime_path.trim() && !draft.library_paths.trim();
  function field(key: keyof EnvironmentDraft, value: string) {
    onDraft({...draft, [key]: value});
  }
  return <div className="environment-section">
    <div className="section-heading"><div><h3 id="environment-heading">OCR environment</h3>
      <p className="muted">Machine-local, saved in App settings, never in portable profiles. Save validates structure only; Check initializes the engine.</p></div>
      <span className={`tag ${dirty ? 'unsaved' : ''}`}>{!loaded ? 'Settings not loaded' : dirty ? 'Unsaved changes' : saved ? 'Saved' : 'Unconfigured'}</span></div>
    <div className="field"><label htmlFor="ocr-profile">Supported OCR profile</label>
      <Select id="ocr-profile" value={draft.profile} disabled={locked} aria-invalid={Boolean(errors.profile)} onChange={value => field('profile', value)}
        options={[{value: '', label: 'Not configured'},
          ...SUPPORTED_PROFILES.map(item => ({value: item.profile, label: item.label})),
          ...(draft.profile && !supported ? [{value: draft.profile, label: `Unsupported · ${draft.profile}`, disabled: true}] : [])]}/>
      {errors.profile && <p className="field-error">{errors.profile}</p>}</div>
    <dl className="fixed-facts">
      <dt>Model</dt><dd>{supported?.model ?? '—'}</dd>
      <dt>Language</dt><dd>{ENVIRONMENT_LANGUAGE}</dd>
      <dt>Provider</dt><dd>{ENVIRONMENT_PROVIDER}</dd>
      <dt>Runtime profile</dt><dd>{ENVIRONMENT_RUNTIME_PROFILE}</dd>
    </dl>
    {saved && fixedMismatch && <p className="inline-warning">The saved environment carries a tuple this desktop build does not offer ({saved.model} / {saved.language} / {saved.provider} / {saved.runtime_profile}). Saving replaces it with the supported tuple shown above.</p>}
    <div className="field"><label htmlFor="model-root">Model root directory</label>
      <input id="model-root" type="text" value={draft.model_root} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.model_root)}
        placeholder="Absolute path to the accepted model files" onChange={event => field('model_root', event.target.value)}/>
      {errors.model_root && <p className="field-error">{errors.model_root}</p>}</div>
    <div className="field"><label htmlFor="runtime-path">OCR runtime library</label>
      <input id="runtime-path" type="text" value={draft.runtime_path} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.runtime_path)}
        placeholder="Absolute path to the pinned ONNX runtime library" onChange={event => field('runtime_path', event.target.value)}/>
      {errors.runtime_path && <p className="field-error">{errors.runtime_path}</p>}</div>
    <div className="field"><label htmlFor="library-paths">Reviewed native library paths · one per line</label>
      <textarea id="library-paths" className="library-paths" value={draft.library_paths} disabled={locked} spellCheck={false} rows={3} aria-invalid={Boolean(errors.library_paths)}
        placeholder="Explicit non-system libraries the engine child may load" onChange={event => field('library_paths', event.target.value)}/>
      {errors.library_paths && <p className="field-error">{errors.library_paths}</p>}
      <p className="field-help">Enter 1–64 absolute paths; blank lines are ignored. Rust canonicalizes them and derives file and SDK identities during Check and again on every Start. Listing a path is not an audit of every loaded image.</p></div>
    <div className="button-row">
      <button id="clear-environment" disabled={locked || blank} onClick={() => onDraft({profile: '', model_root: '', runtime_path: '', library_paths: ''})}>Clear draft</button>
      <span className="muted">{blank ? 'Saving a blank draft stores no OCR environment.' : 'Save changes below writes this environment with the other App settings.'}</span>
    </div>
    <div className="check-section">
      <h3>Check saved environment</h3>
      <dl className="run-identity check-target">
        <dt>Workspace</dt><dd>{target.workspace ? <>{target.label} · revision {target.workspace.revision}</> : 'None selected · validation only'}</dd>
        <dt>Descriptor</dt><dd>{target.descriptorPath ?? 'None · set the recorded corpus descriptor on the workspace Run page'}</dd>
        <dt>Package</dt><dd><code>{target.packageInventoryIdentity ?? 'None inspected'}</code></dd>
      </dl>
      <div className="run-buttons"><button id="check-environment" className="primary" disabled={checkBlock !== null || locked} onClick={onCheck}>Check saved environment</button></div>
      <p className="muted" id="check-block">{checkBlock ?? (target.descriptorPath ? 'Validates files, then initializes the engine against the selected corpus without evaluating package code. Stop applies.' : 'Without a descriptor, Check validates files and reports the missing corpus; it cannot claim initialization.')}</p>
      {lastCheck ? <CheckCard key={lastCheck.association.operation} check={lastCheck} stale={stale} originLabel={originLabel}/> : <p className="muted">No check has completed in this session.</p>}
    </div>
  </div>;
}

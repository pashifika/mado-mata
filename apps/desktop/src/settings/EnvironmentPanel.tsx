import {useState} from 'react';
import Select from '../components/Select.tsx';
import {DISCLOSURE_LIMIT, ENVIRONMENT_LANGUAGE, ENVIRONMENT_PROVIDER, ENVIRONMENT_RUNTIME_PROFILE, SUPPORTED_PROFILES, boundedText, cleanupLabel, faultSummary, initializationLabel, record, text} from '../state.ts';
import type {CheckAssociation, EnvironmentDraft} from '../state.ts';
import type {ControllerView, OcrEnvironment, WorkspaceRef} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export interface LastCheck {association: CheckAssociation; view: ControllerView}

// What a new Check would bind: the selected workspace's identity and corpus, or nothing.
export interface CheckTarget {workspace: WorkspaceRef | null; label: string; descriptorPath: string | null; packageInventoryIdentity: string | null}

interface Props {
  draft: EnvironmentDraft; errors: Record<string, string>; onDraft: (next: EnvironmentDraft) => void;
  saved: OcrEnvironment | null; loaded: boolean; dirty: boolean; locked: boolean; active: boolean; pickerBusy: boolean;
  // Label of the package/workspace host command that keeps Check unavailable; editing the draft stays possible.
  busyReason: string | null;
  target: CheckTarget; onCheck: () => void;
  lastCheck: LastCheck | null; stale: string[];
  originLabel: (workspaceId: string | null) => string;
}

function CheckCard({check, stale, originLabel}: {check: LastCheck; stale: string[]; originLabel: Props['originLabel']}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const {view, association} = check;
  const [disclosed, setDisclosed] = useState(false);
  const result = view.result;
  const source = result ?? record(view.error?.context);
  const completed = Array.isArray(source.completed_stages) ? source.completed_stages.map(String) : [];
  const failure = view.error ?? record(result?.primary);
  const hasFailure = typeof failure.category === 'string';
  const diagnostic = disclosed ? boundedText(JSON.stringify(failure, null, 2), DISCLOSURE_LIMIT) : null;
  return <div className="check-card" id="last-check">
    <div className="panel-heading"><strong>{t.environment.lastCheck}</strong>
      <span className={`tag ${stale.length ? 'stale' : 'current'}`}>{stale.length ? t.environment.stale : t.environment.current}</span></div>
    <dl className="run-identity">
      <dt>{t.common.operation}</dt><dd><code>{association.operation}</code></dd>
      <dt>{t.common.outcome}</dt><dd id="check-outcome">{result ? String(result.status ?? t.common.settled) : view.error ? faultSummary(view.error, true) : t.common.unsettled}{result && hasFailure && <> · {faultSummary(failure, true)}</>}</dd>
      <dt>{t.common.stage}</dt><dd>{text(source.stage) ?? t.common.unobserved}</dd>
      <dt>{t.common.completed}</dt><dd>{completed.length ? completed.join(' → ') : t.common.noneRecorded}</dd>
      <dt>{t.common.initialization}</dt><dd>{initializationLabel(view.progress, locale)}</dd>
      <dt>{t.common.selection}</dt><dd><code>{text(source.selection_identity) ?? t.common.unobserved}</code></dd>
      <dt>{t.common.environment}</dt><dd><code>{text(source.environment_identity) ?? t.common.notDerived}</code></dd>
      <dt>{t.common.corpus}</dt><dd><code>{text(source.corpus_identity) ?? t.common.notDerived}</code></dd>
      <dt>{t.common.workspace}</dt><dd>{association.workspace ? t.common.revision(originLabel(association.workspace.workspace_id), association.workspace.revision) : t.environment.validationOnly}</dd>
      <dt>{t.common.descriptor}</dt><dd>{association.descriptorPath ?? t.environment.noDescriptor}</dd>
      <dt>{t.common.package}</dt><dd><code>{association.packageInventoryIdentity ?? t.common.noneInspected}</code></dd>
      <dt>{t.common.cleanup}</dt><dd>{cleanupLabel(result, source, locale)}</dd>
    </dl>
    {hasFailure && <div className="private-disclosure">
      <button onClick={() => setDisclosed(!disclosed)}>{disclosed ? t.environment.hideDiagnostic : t.environment.discloseDiagnostic}</button>
      {diagnostic && <><pre>{diagnostic.text}</pre>{diagnostic.truncated > 0 && <p className="inline-warning">{t.environment.truncated(diagnostic.truncated)}</p>}</>}
    </div>}
    {stale.length > 0 && <p className="inline-warning">{t.environment.staleHelp(stale)}</p>}
    <p className="authority-note">{t.environment.authority}</p>
  </div>;
}

export default function EnvironmentPanel(props: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const {draft, errors, onDraft, saved, loaded, dirty, locked, active, pickerBusy, busyReason, target, onCheck, lastCheck, stale, originLabel} = props;
  const supported = SUPPORTED_PROFILES.find(item => item.profile === draft.profile);
  const savedModel = saved ? SUPPORTED_PROFILES.find(item => item.profile === saved.profile)?.model : undefined;
  const fixedMismatch = saved !== null && (saved.language !== ENVIRONMENT_LANGUAGE || saved.provider !== ENVIRONMENT_PROVIDER || saved.runtime_profile !== ENVIRONMENT_RUNTIME_PROFILE || saved.model !== savedModel);
  const checkBlock = !loaded ? t.environment.notLoaded : !saved ? t.environment.saveFirst
    : dirty ? t.environment.dirty : active ? t.environment.active
    : pickerBusy ? t.environment.wait(t.target.choosing)
    : busyReason ? t.environment.wait(busyReason) : null;
  const blank = !draft.profile && !draft.model_root.trim() && !draft.runtime_path.trim() && !draft.library_paths.trim();
  function field(key: keyof EnvironmentDraft, value: string) {
    onDraft({...draft, [key]: value});
  }
  return <div className="environment-section">
    <div className="section-heading"><div><h3 id="environment-heading">{t.common.environment}</h3>
      <p className="muted">{t.environment.introduction}</p></div>
      <span className={`tag ${dirty ? 'unsaved' : ''}`}>{!loaded ? t.environment.settingsNotLoaded : dirty ? t.common.unsavedChanges : saved ? t.common.saved : t.common.unconfigured}</span></div>
    <div className="field"><label htmlFor="ocr-profile">{t.environment.supportedProfile}</label>
      <Select id="ocr-profile" value={draft.profile} disabled={locked} aria-invalid={Boolean(errors.profile)} onChange={value => field('profile', value)}
        options={[{value: '', label: t.environment.notConfigured},
          ...SUPPORTED_PROFILES.map(item => ({value: item.profile, label: t.environment.profileLabel(item.profile)})),
          ...(draft.profile && !supported ? [{value: draft.profile, label: t.environment.unsupported(draft.profile), disabled: true}] : [])]}/>
      {errors.profile && <p className="field-error">{errors.profile}</p>}</div>
    <dl className="fixed-facts">
      <dt>{t.common.model}</dt><dd>{supported?.model ?? '—'}</dd>
      <dt>{t.common.language}</dt><dd>{ENVIRONMENT_LANGUAGE}</dd>
      <dt>{t.common.provider}</dt><dd>{ENVIRONMENT_PROVIDER}</dd>
      <dt>{t.common.runtimeProfile}</dt><dd>{ENVIRONMENT_RUNTIME_PROFILE}</dd>
    </dl>
    {saved && fixedMismatch && <p className="inline-warning">{t.environment.mismatch(saved.model, saved.language, saved.provider, saved.runtime_profile)}</p>}
    <div className="field"><label htmlFor="model-root">{t.environment.modelRoot}</label>
      <input id="model-root" type="text" value={draft.model_root} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.model_root)}
        placeholder={t.environment.modelPlaceholder} onChange={event => field('model_root', event.target.value)}/>
      {errors.model_root && <p className="field-error">{errors.model_root}</p>}</div>
    <div className="field"><label htmlFor="runtime-path">{t.environment.runtime}</label>
      <input id="runtime-path" type="text" value={draft.runtime_path} disabled={locked} spellCheck={false} aria-invalid={Boolean(errors.runtime_path)}
        placeholder={t.environment.runtimePlaceholder} onChange={event => field('runtime_path', event.target.value)}/>
      {errors.runtime_path && <p className="field-error">{errors.runtime_path}</p>}</div>
    <div className="field"><label htmlFor="library-paths">{t.environment.libraries}</label>
      <textarea id="library-paths" className="library-paths" value={draft.library_paths} disabled={locked} spellCheck={false} rows={3} aria-invalid={Boolean(errors.library_paths)}
        placeholder={t.environment.librariesPlaceholder} onChange={event => field('library_paths', event.target.value)}/>
      {errors.library_paths && <p className="field-error">{errors.library_paths}</p>}
      <p className="field-help">{t.environment.librariesHelp}</p></div>
    <div className="button-row">
      <button id="clear-environment" disabled={locked || blank} onClick={() => onDraft({profile: '', model_root: '', runtime_path: '', library_paths: ''})}>{t.environment.clear}</button>
      <span className="muted">{blank ? t.environment.blank : t.environment.saveHelp}</span>
    </div>
    <div className="check-section">
      <h3>{t.environment.check}</h3>
      <dl className="run-identity check-target">
        <dt>{t.common.workspace}</dt><dd>{target.workspace ? t.common.revision(target.label, target.workspace.revision) : t.environment.noSelection}</dd>
        <dt>{t.common.descriptor}</dt><dd>{target.descriptorPath ?? t.environment.setDescriptor}</dd>
        <dt>{t.common.package}</dt><dd><code>{target.packageInventoryIdentity ?? t.common.noneInspected}</code></dd>
      </dl>
      <div className="run-buttons"><button id="check-environment" className="primary" disabled={checkBlock !== null || locked} onClick={onCheck}>{t.environment.check}</button></div>
      <p className="muted" id="check-block">{checkBlock ?? (target.descriptorPath ? t.environment.checkHelp : t.environment.noCorpus)}</p>
      {lastCheck ? <CheckCard key={lastCheck.association.operation} check={lastCheck} stale={stale} originLabel={originLabel}/> : <p className="muted">{t.environment.noChecks}</p>}
    </div>
  </div>;
}

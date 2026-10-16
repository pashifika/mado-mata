import {useState} from 'react';
import Select from '../components/Select.tsx';
import {DISCLOSURE_LIMIT, boundedText, cleanupLabel, faultSummary, initializationLabel, record, text} from '../state.ts';
import type {CheckAssociation, EnvironmentDraft} from '../state.ts';
import type {ControllerView, OcrEnvironment, WorkspaceRef} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import ResourceSetupPanel from './ResourceSetupPanel.tsx';
import {evaluationCount, evaluationTags, profileChoice, selectProfile} from './ocr-setup.ts';
import type {OcrProfileChoice} from './ocr-setup.ts';
import type {OcrSetupControls} from './useOcrSetup.ts';

export interface LastCheck {association: CheckAssociation; view: ControllerView}

// What a new Check would bind: the selected workspace's identity and corpus, or nothing.
export interface CheckTarget {workspace: WorkspaceRef | null; label: string; descriptorPath: string | null; packageInventoryIdentity: string | null}

interface Props {
  draft: EnvironmentDraft; errors: Record<string, string>; onDraft: (next: EnvironmentDraft) => void;
  saved: OcrEnvironment | null; loaded: boolean; dirty: boolean; locked: boolean; active: boolean; pickerBusy: boolean;
  // Label of the package/workspace host command that keeps Check unavailable; editing the draft stays possible.
  busyReason: string | null;
  // The application-wide Edit lease refuses an independent Check until Edit ends.
  authoringReason: string | null;
  target: CheckTarget; onCheck: () => void;
  lastCheck: LastCheck | null; stale: string[];
  originLabel: (workspaceId: string | null) => string;
  setup: OcrSetupControls;
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

// Recorded SDK report results; every rule and its provenance stays visible, including failures and missing runs.
function Evaluations({choice, target}: {choice: OcrProfileChoice; target: string}) {
  const t = messages[useLocale()].ui.environment;
  return <div className="ocr-model-evaluations">
    <p className="field-help">{t.evaluationsHelp}</p>
    {choice.evaluations.length === 0 && <p className="muted">{t.noEvaluations}</p>}
    {choice.evaluations.map(summary => <section key={`${summary.target}:${summary.provider}`} className="ocr-model-evaluation">
      <div className="section-heading"><strong>{summary.target} · {summary.provider}</strong>
        {summary.target === target && summary.provider === choice.provider && <span className="tag">{t.thisMachine}</span>}</div>
      <dl className="fixed-facts">
        <dt>{t.criteria}</dt><dd>{summary.criteria}</dd>
        <dt>{t.report}</dt><dd>{summary.evidence?.report ?? t.noEvidence}</dd>
        {summary.evidence && <>
          <dt>{t.measuredRevision}</dt><dd>{summary.evidence.measured_source_revision}</dd>
          <dt>{t.executionDigest}</dt><dd>{summary.evidence.execution_sha256}</dd>
          <dt>{t.evaluatorRevision}</dt><dd>{summary.evidence.evaluator_revision}</dd>
        </>}
      </dl>
      <ul className="ocr-model-rules">{summary.rules.map(rule => <li key={rule.rule_id}><code>{rule.rule_id}</code>
        <span className={`tag ${rule.outcome === 'passed' ? 'current' : rule.outcome === 'not_run' ? '' : 'stale'}`}>{t.outcome(rule.outcome)}</span></li>)}</ul>
    </section>)}
  </div>;
}

export default function EnvironmentPanel(props: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const {draft, errors, onDraft, saved, loaded, dirty, locked, active, pickerBusy, busyReason, authoringReason, target, onCheck, lastCheck, stale, originLabel, setup} = props;
  const view = setup.state.view;
  const supported = view && profileChoice(view.profiles, draft.profile);
  const savedChoice = saved && view ? profileChoice(view.profiles, saved.profile) : null;
  const fixedMismatch = saved !== null && view !== null && (savedChoice === null || saved.model !== savedChoice.model || saved.language !== savedChoice.language
    || saved.provider !== savedChoice.provider || saved.runtime_profile !== savedChoice.runtime_profile);
  const count = supported && view ? evaluationCount(supported, view.target) : null;
  const choices = view ? view.profiles.map(choice => {
    const variant = t.environment.variant(choice.variant);
    const tags = evaluationTags(choice, view.target);
    const label = [choice.display_name, variant, ...tags.map(tag => t.environment.tags[tag].label)].filter(Boolean).join(' · ');
    return {value: choice.profile, label, title: label, content: <span className="ocr-model-choice">
      <span className="ocr-model-name">{choice.display_name}</span>
      <span className="ocr-model-secondary"><span className="ocr-model-variant">{variant}</span>
        <span className="ocr-model-tags">{tags.map(tag => <span key={tag}
          className={`tag ${tag === 'fast' ? 'ocr-fast' : tag === 'reading_good' ? 'current' : tag.endsWith('_attention') ? 'stale' : ''}`}
          title={t.environment.tags[tag].help}>{t.environment.tags[tag].label}</span>)}</span>
      </span>
    </span>};
  }) : [];
  const checkBlock = !loaded ? t.environment.notLoaded : !saved ? t.environment.saveFirst
    : view === null ? setup.state.error === null ? t.ocrSetup.loading : messages[locale].validation.profilesUnavailable
    : dirty ? t.environment.dirty : active ? t.environment.active : setup.busy ? t.ocrSetup.busy
    : authoringReason ?? (pickerBusy ? t.environment.wait(t.target.choosing)
      : busyReason ? t.environment.wait(busyReason) : null);
  const setupBlock = !loaded ? t.environment.notLoaded : locked ? t.ocrSetup.saving : active ? t.environment.active
    : authoringReason ?? (pickerBusy ? t.settings.wait(t.target.choosing) : busyReason ? t.settings.wait(busyReason) : null);
  const blank = !draft.profile && !draft.model_root.trim() && !draft.runtime_path.trim() && !draft.library_paths.trim();
  function field(key: keyof EnvironmentDraft, value: string) {
    onDraft({...draft, [key]: value});
  }
  return <div className="environment-section">
    <div className="section-heading"><div><h3 id="environment-heading">{t.common.environment}</h3>
      <p className="muted">{t.environment.introduction}</p></div>
      <span className={`tag ${dirty ? 'unsaved' : ''}`}>{!loaded ? t.environment.settingsNotLoaded : dirty ? t.common.unsavedChanges : saved ? t.common.saved : t.common.unconfigured}</span></div>
    <ResourceSetupPanel setup={setup} block={setupBlock}/>
    <div className="field"><label htmlFor="ocr-profile">{t.environment.model}</label>
      <Select id="ocr-profile" value={draft.profile} disabled={locked || view === null} aria-invalid={Boolean(errors.profile)} aria-describedby="ocr-model-tag-help"
        title={choices.find(choice => choice.value === draft.profile)?.title} onChange={value => onDraft(selectProfile(draft, value, view))}
        options={[{value: '', label: t.environment.notConfigured}, ...choices,
          ...(draft.profile && !supported ? [{value: draft.profile, label: view ? t.environment.unsupported(draft.profile) : draft.profile, disabled: true}] : [])]}/>
      {errors.profile && <p className="field-error">{errors.profile}</p>}</div>
    <p className="field-help" id="ocr-model-tag-help">{t.environment.tagsHelp}</p>
    <details className="ocr-setup-details"><summary>{t.environment.technicalHeading}</summary>
      {supported && view ? <>
        <dl className="fixed-facts">
          <dt>{t.environment.model}</dt><dd>{supported.display_name}</dd>
          <dt>{t.environment.variantLabel}</dt><dd>{t.environment.variant(supported.variant) || '—'}</dd>
          <dt>{t.environment.resultsSummary}</dt><dd>{count ? t.environment.evaluationCount(count.passed, count.total) : t.environment.outcome('not_run')}</dd>
          <dt>{t.common.model}</dt><dd>{supported.model}</dd>
          <dt>{t.common.profile}</dt><dd>{supported.profile}</dd>
          <dt>{t.common.language}</dt><dd>{supported.language}</dd>
          <dt>{t.common.provider}</dt><dd>{supported.provider}</dd>
          <dt>{t.common.runtimeProfile}</dt><dd>{supported.runtime_profile}</dd>
          <dt>{t.environment.preprocessing}</dt><dd>{supported.preprocessing}</dd>
          <dt>{t.environment.modelFiles}</dt><dd>{supported.resource_id}</dd>
        </dl>
        <Evaluations choice={supported} target={view.target}/>
      </> : <p className="muted">—</p>}
    </details>
    {saved && fixedMismatch && <p className="inline-warning">{t.environment.mismatch(saved.model, saved.language, saved.provider, saved.runtime_profile)}</p>}
    <details className="ocr-manual" open={fixedMismatch || Boolean((draft.model_root.trim() && errors.model_root) || (draft.runtime_path.trim() && errors.runtime_path) || (draft.library_paths.trim() && errors.library_paths))}>
      <summary>{t.environment.manualHeading}</summary>
      <p className="field-help">{t.environment.manualHelp}</p>
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
      <button type="button" disabled={setupBlock !== null || setup.busy || setup.state.catalogPending || setup.choice === null} onClick={setup.manual}>{t.environment.checkManual}</button>
      <button id="clear-environment" disabled={locked || blank} onClick={() => onDraft({profile: '', model_root: '', runtime_path: '', library_paths: ''})}>{t.environment.clear}</button>
      <span className="muted">{blank ? t.environment.blank : t.environment.saveHelp}</span>
    </div>
    </details>
    <div className="check-section">
      <h3>{t.environment.check}</h3>
      <dl className="run-identity check-target">
        <dt>{t.common.workspace}</dt><dd>{target.workspace ? t.common.revision(target.label, target.workspace.revision) : t.environment.noSelection}</dd>
        <dt>{t.common.descriptor}</dt><dd>{target.descriptorPath ?? t.environment.setDescriptor}</dd>
        <dt>{t.common.package}</dt><dd><code>{target.packageInventoryIdentity ?? t.common.noneInspected}</code></dd>
      </dl>
      <div className="run-buttons"><button id="check-environment" className="primary" disabled={checkBlock !== null || locked}
        aria-describedby="check-block"
        onClick={onCheck}>{t.environment.check}</button></div>
      <p className={checkBlock !== null ? 'inline-info' : 'muted'}
        id="check-block">{checkBlock ?? (target.descriptorPath ? t.environment.checkHelp : t.environment.noCorpus)}</p>
      {lastCheck ? <CheckCard key={lastCheck.association.operation} check={lastCheck} stale={stale} originLabel={originLabel}/> : <p className="muted">{t.environment.noChecks}</p>}
    </div>
  </div>;
}

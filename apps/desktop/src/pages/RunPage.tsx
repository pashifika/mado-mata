import {useEffect, useRef, useState} from 'react';
import SchemaForm from '../components/SchemaForm.tsx';
import Select from '../components/Select.tsx';
import ResultPanel, {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {faultSummary, text} from '../state.ts';
import type {CheckAssociation} from '../state.ts';
import {DESCRIPTOR_LIMIT, busy, editDraft} from '../workspace.ts';
import type {Workspace} from '../workspace.ts';
import type {ControllerView, Json, OcrEnvironment, Profile} from '../types.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Immutable facts captured when this frontend submitted the operation; the host view is authoritative.
export type RunSnapshot =
  | {kind: 'run'; run: string; lane: string; packageId: string; profileName: string; profileId: string; scenario: string; descriptorPath: string | null; values: Record<string, Json>}
  | {kind: 'check'; run: string; association: CheckAssociation};

export interface RunView {view: ControllerView; live: boolean; olderRevision: number | null}

export interface Derived {
  parsed: {values: Record<string, Json>; errors: Record<string, string>};
  numericErrors: boolean; valuesDirty: boolean; dirty: boolean; bound: boolean;
  selectedProfile: Profile | undefined; startBlock: string | null; descriptorError: string | null;
}

export interface RunHandlers {
  change: (update: (workspace: Workspace) => Workspace) => void;
  validate: () => void; saveProfile: () => void; renameProfile: () => void; deleteProfile: () => void;
  newDraft: (preset?: string) => void; selectProfile: (id: string) => void;
  reinspect: () => void; start: () => void; stop: () => void;
}

interface Props {
  workspace: Workspace; label: string; derived: Derived; run: RunView; snapshot: RunSnapshot | null;
  locked: boolean; active: boolean; starting: boolean; stopping: boolean; closing: boolean;
  savedEnvironment: OcrEnvironment | null; handlers: RunHandlers;
}

export default function RunPage({workspace, label, derived, run, snapshot, locked, active, starting, stopping, closing, savedEnvironment, handlers}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const {parsed, numericErrors, valuesDirty, dirty, bound, selectedProfile, startBlock, descriptorError} = derived;
  const [confirmReinspect, setConfirmReinspect] = useState(false);
  const reinspectButton = useRef<HTMLButtonElement>(null);
  const confirmRow = useRef<HTMLDivElement>(null);
  // Set only when the confirmation row closes while it owns focus. Focus returns to Reinspect; once the submitted
  // command locks that button (and a fresh revision remounts this page), the stable Run control page tab takes it.
  const refocus = useRef(false);
  useEffect(() => {
    if (confirmReinspect || !refocus.current) return;
    refocus.current = false;
    const button = reinspectButton.current;
    (button && !button.disabled ? button : document.getElementById('page-run'))?.focus();
  }, [confirmReinspect]);
  function closeConfirm(reinspect: boolean) {
    refocus.current = confirmRow.current?.contains(document.activeElement) ?? false;
    setConfirmReinspect(false);
    if (reinspect) handlers.reinspect();
  }
  const view = run.view;
  const phase = starting ? 'preparing' : view.state;
  const check = view.operation === 'environment_check';
  const primary = view.error ?? (view.result?.primary ? fault(view.result.primary) : null);
  const privatePrimary = !check && (snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane)) !== 'controlled';
  const selectedDescriptor = workspace.descriptorPath.trim() || null;
  const canStart = !locked && !active && !numericErrors && bound && startBlock === null && descriptorError === null;
  const stopAvailable = run.live && view.run !== null && busy(view.state) && view.state !== 'stopping' && !stopping && !starting && !closing;
  const executionCaption = starting ? t.run.submitted : run.live && busy(view.state) ? t.run.owned(view.run) : view.state === 'terminal' ? t.run.settled(view.run) : t.run.noOperations;
  const heading = selectedProfile && !valuesDirty ? selectedProfile.name : workspace.name || t.run.untitled;
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.run.heading(label)}</span><h1>{heading}</h1>
      <p>{t.run.introduction}</p></div>
      <div className="actions">
        <button id="validate" disabled={locked || numericErrors || !bound} onClick={handlers.validate}>{t.run.validate}</button>
        <button id="start" className="primary" disabled={!canStart} onClick={handlers.start}>{t.run.start}</button>
      </div>
    </div>
    <div id="error">
      {workspace.error && <FaultMessage title={t.run.actionFailed} value={workspace.error}/>}
      {primary && (privatePrimary || check
        ? <section className="fault" role="alert"><strong>{check ? t.run.checkError : t.run.runError} · {faultSummary(primary, !privatePrimary)}</strong>
            <p>{t.run.diagnosticHelp}</p></section>
        : <FaultMessage title={t.run.runError} value={primary}/>)}
    </div>
    <div className="operation-status" role="status">{renderMessage(locale, workspace.busy ?? workspace.notice) || (startBlock ?? '')}</div>
    <section className="panel summary-panel" aria-label={t.run.summary}>
      <div className="summary-item"><span className="eyebrow">{t.common.execution}</span>
        <div className="state-line"><span className={`dot phase-${phase}`} aria-hidden="true"/><span id="state" className={`phase phase-${phase}`}>{t.phase(phase)}</span></div>
        <p className="summary-caption">{executionCaption}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.common.profile}</span>
        <div className="summary-value">{selectedProfile ? selectedProfile.name : workspace.name || t.run.untitled}</div>
        <p className={`summary-caption save-status ${dirty ? 'unsaved' : ''}`}>{selectedProfile ? dirty ? t.run.profileChanged : t.run.profileSaved : t.run.profileDraft}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.run.inputRoute}</span>
        <div className="summary-value">{t.lane(workspace.lane)}</div>
        <p className="summary-caption">{workspace.lane === 'replay' ? t.run.replaySummary : t.run.controlledSummary}</p></div>
    </section>
    <div className="run-grid">
      <section className="panel" aria-labelledby="config-heading">
        <div className="panel-heading"><h2 id="config-heading">{t.run.configuration}</h2><span className="tag">{dirty ? t.common.unsavedChanges : t.run.savedValues}</span></div>
        <div className="panel-body">
          <div className="package-card">
            <div className="package-text"><strong>{workspace.package.package_id}</strong>
              <span className="mono" title={workspace.packagePath}>{workspace.package.runtime} · {workspace.packagePath}</span></div>
            <span className="tag">{t.run.inspected(workspace.revision)}</span>
            {confirmReinspect
              ? <div ref={confirmRow} className="confirm-row" role="alertdialog" aria-labelledby="confirm-reinspect-text">
                <span id="confirm-reinspect-text">{t.run.confirmReinspect}</span>
                <button type="button" className="danger-text" onClick={() => closeConfirm(true)}>{t.run.discardReinspect}</button>
                <button type="button" autoFocus onClick={() => closeConfirm(false)}>{t.run.keepDraft}</button></div>
              : <button id="reinspect" ref={reinspectButton} disabled={locked || starting || (run.live && busy(view.state))} title={run.live && busy(view.state) ? t.run.reinspectBlocked : undefined}
                onClick={() => dirty && workspace.touched ? setConfirmReinspect(true) : handlers.reinspect()}>{t.run.reinspect}</button>}
          </div>
          <p className="field-help">{t.run.reinspectHelp}</p>
          {workspace.profilesError && <><FaultMessage title={t.run.profilesError} value={workspace.profilesError}/><p className="muted">{t.run.profilesHelp}</p></>}
          <div className="two-col">
            <div className="field"><label htmlFor="profile-select">{t.run.savedProfile}</label>
              <Select id="profile-select" value={workspace.selectedId ?? ''} disabled={locked} onChange={handlers.selectProfile}
                options={[{value: '', label: t.run.unsavedDraft}, ...workspace.profiles.map(profile => ({value: profile.id, label: profile.name}))]}/>
              {selectedProfile && <p className="identity-text">{selectedProfile.id}</p>}
              {!bound && <p className="inline-warning">{t.run.staleSchema}</p>}</div>
            <div className="field"><label htmlFor="preset-select">{t.run.preset}</label>
              <Select id="preset-select" value={workspace.preset} disabled={locked} onChange={handlers.newDraft}
                options={[{value: '', label: t.run.defaults}, ...Object.keys(workspace.package.profiles).map(key => ({value: key, label: key}))]}/>
              <p className="field-help">{t.run.presetHelp}</p></div>
          </div>
          <div className="field"><label htmlFor="profile-name">{t.run.profileName}</label>
            <input id="profile-name" value={workspace.name} disabled={locked} onChange={event => handlers.change(item => ({...item, name: event.target.value, notice: null, touched: true}))}/></div>
          <div className="button-row">
            <button id="save-profile" className="primary" disabled={locked || !workspace.name.trim() || numericErrors || !bound} onClick={handlers.saveProfile}>{workspace.selectedId ? t.run.updateProfile : t.run.saveProfile}</button>
            <button id="new-profile" disabled={locked} onClick={() => handlers.newDraft()}>{t.run.newDraft}</button>
            <button id="rename-profile" disabled={!workspace.selectedId || locked || !workspace.name.trim() || !bound} onClick={handlers.renameProfile}>{t.run.rename}</button>
            <button id="delete-profile" className="danger-text" disabled={!workspace.selectedId || locked} onClick={handlers.deleteProfile}>{t.run.deleteProfile}</button>
          </div>
          <h3>{t.run.options}</h3>
          <p className="field-help">{t.run.optionsHelp}</p>
          <SchemaForm schema={workspace.package.schema} value={workspace.draft} onChange={draft => handlers.change(item => editDraft(item, draft))} errors={parsed.errors}/>
          {workspace.validation && <details className="validated"><summary>{t.run.valid}</summary><pre>{JSON.stringify(workspace.validation, null, 2)}</pre></details>}
          <h3>{t.run.route}</h3>
          <div className="two-col">
            <div className="field"><label htmlFor="lane">{t.run.lane}</label>
              <Select id="lane" value={workspace.lane} disabled={locked} onChange={lane => handlers.change(item => ({...item, lane, notice: null}))}
                options={[{value: 'controlled', label: t.run.controlled},
                  {value: 'replay', label: t.run.replay},
                  {value: 'native', label: t.run.native, disabled: true}]}/></div>
            <div className="field"><label htmlFor="scenario">{t.run.scenario}</label>
              <Select id="scenario" value={workspace.lane === 'replay' ? 'workflow' : workspace.scenario} disabled={locked || workspace.lane === 'replay'}
                onChange={scenario => handlers.change(item => ({...item, scenario}))}
                options={[{value: 'workflow', label: t.run.workflow}, {value: 'held-work', label: t.run.heldWork}, {value: 'no-match', label: t.run.noMatch}]}/></div>
          </div>
          <div className="field"><label htmlFor="descriptor-path">{t.run.descriptor}</label>
            <input id="descriptor-path" type="text" value={workspace.descriptorPath} disabled={locked} spellCheck={false} aria-invalid={descriptorError !== null}
              placeholder={t.run.descriptorPlaceholder} onChange={event => handlers.change(item => ({...item, descriptorPath: event.target.value, notice: null}))}/>
            {descriptorError && <p className="field-error">{descriptorError}</p>}
            <p className="field-help">{t.run.descriptorHelp(DESCRIPTOR_LIMIT)}</p></div>
          <p className="muted">{t.run.startUses(selectedProfile && !valuesDirty ? selectedProfile.name : null, workspace.lane === 'replay', savedEnvironment?.profile ?? null, selectedDescriptor)}</p>
          {startBlock && <p className="inline-warning" id="start-block">{startBlock}</p>}
        </div>
      </section>
      <section className="panel" aria-labelledby="run-heading">
        <div className="panel-heading"><div><span className="eyebrow">{t.run.immutable}</span><h2 id="run-heading">{check ? t.run.environmentCheck : t.common.execution}</h2></div>
          <span className={`phase phase-${phase}`}>{t.phase(phase)}</span></div>
        <div className="panel-body">
          {run.olderRevision !== null && <p className="inline-warning">{t.run.olderRevision(run.olderRevision, workspace.revision)}</p>}
          <div className="run-buttons">
            <button id="stop" className="stop-button" disabled={!stopAvailable} onClick={handlers.stop}>{stopping ? t.run.requestingStop : t.run.stop}</button>
            <span className="muted">{t.run.stopTarget(run.live && view.run ? view.run : null)}</span>
          </div>
          <p className="authority-note">{t.run.authority}</p>
          <dl className="run-identity"><dt>{t.run.operationId}</dt><dd id="run-id">{view.run ?? t.common.noOperation}</dd>
            <dt>{t.run.kind}</dt><dd id="operation-kind">{view.run ? check ? t.run.checkKind : t.run.runKind(t.lane(snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane) ?? t.common.unknown)) : t.phase('idle')}</dd>
            {snapshot?.kind === 'run' && snapshot.run === view.run && <><dt>{t.run.capturedProfile}</dt><dd>{snapshot.profileName || t.run.untitled} · {snapshot.profileId}</dd><dt>{t.run.packageScenario}</dt><dd>{snapshot.packageId} / {snapshot.scenario}</dd>
              {snapshot.lane === 'replay' && <><dt>{t.common.descriptor}</dt><dd>{snapshot.descriptorPath}</dd></>}</>}
            {snapshot?.kind === 'check' && snapshot.run === view.run && <><dt>{t.run.checkedProfile}</dt><dd>{snapshot.association.environment?.profile ?? t.common.unconfigured}</dd>
              <dt>{t.common.descriptor}</dt><dd>{snapshot.association.descriptorPath ?? t.run.noInitialization}</dd></>}
          </dl>
          {snapshot?.kind === 'run' && snapshot.run === view.run && <details><summary>{t.run.capturedOptions}</summary><pre>{JSON.stringify(snapshot.values, null, 2)}</pre></details>}
          {snapshot?.kind === 'check' && snapshot.run === view.run && <details><summary>{t.run.capturedEnvironment}</summary><pre>{JSON.stringify(snapshot.association.environment, null, 2)}</pre></details>}
          <h3>{t.run.milestones}</h3><ol className="progress-list">{view.progress.map((event, index) => <li key={`${view.run}-${index}`}>
            <details><summary>{String(event.event ?? t.run.milestone)}{text(event.stage) ? ` · ${event.stage}` : ''}{typeof event.at_us === 'number' ? ` · ${(event.at_us / 1000).toFixed(1)} ms` : ''}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>
          </li>)}</ol>{view.progress.length === 0 && <p className="muted">{t.run.noMilestones}</p>}
          <ResultPanel view={view} disclosed={view.run !== null && workspace.disclosedRun === view.run} onDisclose={next => handlers.change(item => ({...item, disclosedRun: next ? view.run : null}))}/>
        </div>
      </section>
    </div>
  </>;
}

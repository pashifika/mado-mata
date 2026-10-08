import {Fragment, useEffect, useRef, useState} from 'react';
import ProfileRecovery from '../components/ProfileRecovery.tsx';
import type {RecoveryHandlers} from '../components/ProfileRecovery.tsx';
import SchemaForm from '../components/SchemaForm.tsx';
import Select from '../components/Select.tsx';
import TargetPanel from '../components/TargetPanel.tsx';
import type {TargetHandlers} from '../components/TargetPanel.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import {ButtonHint, HelpTrigger} from '../components/ContextualHelp.tsx';
import type {RunView} from './ExecutionPage.tsx';
import {DESCRIPTOR_LIMIT, UNSUPPORTED_SOURCE, busy, chooseLane, editDraft, hasWorkspaceEdits} from '../workspace.ts';
import type {Bound, BoundWorkspace, Derived, NativeConsent} from '../workspace.ts';
import {AUTHORING_RECOVERY, recoveryPath} from '../authoring.ts';
import type {PageAuthoring} from './EditPage.tsx';
import type {Fault, NativeCapability, Settings} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import './run-control.css';

export interface RunHandlers {
  // Draft-affecting edits clear the transient notice.
  change: (update: (bound: Bound) => Bound) => void;
  validate: () => void; saveProfile: () => void; renameProfile: () => void; deleteProfile: () => void;
  newDraft: (preset?: string) => void; selectProfile: (id: string) => void;
  reinspect: () => void; inspectPath: (value: string) => void; importLegacy: () => void; start: () => void;
  // Native review text edits withdraw consent; consent binds to the request as it is now.
  native: {edit: (field: 'operation' | 'postcondition' | 'workflowSeconds', value: string) => void; approve: (field: NativeConsent, value: boolean) => void};
  target: TargetHandlers;
  recovery: RecoveryHandlers;
}

interface Props {
  workspace: BoundWorkspace; label: string; derived: Derived; run: RunView;
  locked: boolean; active: boolean; pickerBusy: boolean; starting: boolean;
  // The saved App settings environment that Replay Start reads; unsaved settings drafts never stand in for it.
  environment: Settings['ocr_environment']; handlers: RunHandlers;
  // Host-issued Native policy; null when this platform/build offers none. `nativeError` is a failed availability read.
  nativeCapability: NativeCapability | null; nativeError: Fault | null;
  // While any Tab owns Edit, Start is refused everywhere; the owner additionally waits to exit before reinspecting.
  authoring: PageAuthoring;
}

export default function RunPage({workspace, label, derived, run, locked, active, pickerBusy, starting, environment, handlers, authoring, nativeCapability, nativeError}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const bound = workspace.bound;
  const {parsed, numericErrors, valuesDirty, dirty, bound: profileBound, selectedProfile, startBlock, descriptorError} = derived;
  const [confirmReinspect, setConfirmReinspect] = useState(false);
  const [confirmEdit, setConfirmEdit] = useState(false);
  const a = t.authoring;
  const editOwner = authoring.role === 'owner';
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
  // Every Reinspect entry point, including the rejected-profile recovery route, passes the same unsaved-edits check.
  function confirmedReinspect() {
    if (editOwner) return;
    if (hasWorkspaceEdits(workspace, derived)) setConfirmReinspect(true); else handlers.reinspect();
  }
  // Leaving Edit invalidates this inspection, so unsaved profile/target drafts are discarded only after confirmation.
  function confirmedEdit() {
    if (authoring.block !== null) return;
    if (hasWorkspaceEdits(workspace, derived)) setConfirmEdit(true); else authoring.onOpen(bound.packagePath);
  }
  const view = run.view;
  const phase = starting ? 'preparing' : view.state;
  const canStart = !locked && !active && !pickerBusy && authoring.role === null && !numericErrors && profileBound && startBlock === null && descriptorError === null;
  const executionCaption = starting ? t.run.submitted : run.live && busy(view.state) ? t.run.owned(view.run) : view.state === 'terminal' ? t.run.settled(view.run) : t.run.noOperations;
  const heading = selectedProfile && !valuesDirty ? selectedProfile.name : bound.name || t.run.untitled;
  const legacy = workspace.legacyImport;
  const native = derived.native;
  const nativeBinding = native?.binding ?? null;
  const nativeInput = nativeBinding?.configuration.input ?? null;
  const recipe = native?.recipe ?? null;
  // Launch recipe validity is conditional on host-confirmed absence, not a gate on attaching.
  const approvalOpen = native !== null && (native.block === null || native.block === 'nativeApproval') && !locked;
  // Invalid Workflow text has no effective duration; never display a clamped or fallback budget.
  const nativeLimits = nativeCapability?.default_limits ?? null;
  const selectedLimits = native?.limits ?? null;
  const nativeBudgetRows = nativeLimits && [
    [t.run.nativeStartup, nativeLimits.startup_ms], [t.run.nativeReadiness, nativeLimits.readiness_ms], [t.run.nativeWorkflow, selectedLimits?.workflow_ms ?? null],
  ] as const;
  const nativeDuration = selectedLimits ? (1 + (native?.recovery ? 1 : 0)) * (selectedLimits.startup_ms + selectedLimits.readiness_ms + selectedLimits.workflow_ms) + (native?.recovery ? 3000 : 0) : null;
  const nativeLimitRows = nativeLimits && [
    [t.run.nativeFrames, String(nativeLimits.max_frames)],
    [t.run.nativeWait, `${nativeLimits.wait_ms} ms`], [t.run.nativeInterval, `${nativeLimits.interval_ms} ms`],
    [t.run.nativeActions, String(nativeLimits.max_actions)], [t.run.nativeCleanup, `${nativeLimits.cleanup_ms} ms`],
    [t.run.nativeContainment, `${nativeLimits.containment_ms} ms`],
  ];
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.run.heading(label)}</span><h1>{heading}<HelpTrigger title={t.run.heading(label)} hint={t.run.introductionHint}>{t.run.introduction}</HelpTrigger></h1></div></div>
    <div id="error">
      {workspace.error && <FaultMessage title={t.run.actionFailed} value={workspace.error}/>}
      {workspace.error?.category === AUTHORING_RECOVERY && <div className="button-row">
        <button id="workspace-recover" type="button" disabled={locked} onClick={() => authoring.onRecover(recoveryPath(workspace.error!, bound.packagePath))}>{a.recover}</button>
        <span className="muted">{a.recoverHelp}</span></div>}
      {workspace.sourceError && <FaultMessage title={workspace.sourceError.category === UNSUPPORTED_SOURCE ? t.guidance.unsupportedHeading : t.guidance.unavailableHeading} value={workspace.sourceError}/>}
    </div>
    <section className="panel summary-panel run-summary" aria-label={t.run.summary}>
      <div className="summary-item"><span className="eyebrow">{t.run.status}</span>
        <div className="state-line"><span className={`dot phase-${phase}`} aria-hidden="true"/><span id="state" className={`phase phase-${phase}`}>{t.phase(phase)}</span></div>
        <p className="summary-caption">{executionCaption}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.run.nextRunProfile}</span>
        <div className="summary-value">{selectedProfile ? selectedProfile.name : bound.name || t.run.untitled}</div>
        <p className={`summary-caption save-status ${dirty ? 'unsaved' : ''}`}>{selectedProfile ? dirty ? t.run.profileChanged : t.run.profileSaved : t.run.profileDraft}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.run.executionMode}<HelpTrigger title={t.run.executionMode} hint={bound.lane === 'replay' ? t.run.replaySummary : bound.lane === 'native' ? t.run.nativeSummary : t.run.controlledSummary}>
        {bound.lane === 'replay' ? t.run.replaySummary : bound.lane === 'native' ? t.run.nativeSummary : t.run.controlledSummary}
      </HelpTrigger></span><div className="summary-value">{t.lane(bound.lane)}</div></div>
      <div className="actions run-summary-actions">
        <ButtonHint hint={t.run.validateHint}><button id="validate" disabled={locked || numericErrors || !profileBound} onClick={handlers.validate}>{t.run.validate}</button></ButtonHint>
        <ButtonHint hint={t.run.startHint}><button id="start" className="primary" disabled={!canStart}
          aria-describedby={['run-start-uses', authoring.role !== null ? 'app-authoring-strip' : null, startBlock ? 'start-block' : null].filter(Boolean).join(' ')}
          onClick={handlers.start}>{t.run.start}</button></ButtonHint>
      </div>
    </section>
    <p id="run-start-uses" className="muted">{t.run.startUses(selectedProfile && !valuesDirty ? selectedProfile.name : null, bound.lane === 'replay', environment?.profile ?? null, bound.descriptorPath.trim() || null)}</p>
    <div className="run-grid">
      <section className="panel" aria-labelledby="config-heading">
        <div className="panel-heading"><h2 id="config-heading">{t.run.configuration}</h2><span className="tag">{dirty ? t.common.unsavedChanges : t.run.savedValues}</span></div>
        <div className="panel-body">
          <span className="eyebrow">{t.run.sourceEyebrow}</span>
          <div className="package-card">
            <div className="package-text"><strong>{bound.package.package_id}</strong>
              <span className="mono" title={bound.packagePath}>{bound.package.runtime} · {bound.packagePath}</span></div>
            <span className="tag">{t.run.inspected(workspace.revision)}</span>
            {confirmReinspect
              ? <div ref={confirmRow} className="confirm-row" role="alertdialog" aria-labelledby="confirm-reinspect-text">
                <span id="confirm-reinspect-text">{t.run.confirmReinspect}</span>
                <button type="button" className="danger-text" onClick={() => closeConfirm(true)}>{t.run.discardReinspect}</button>
                <button type="button" autoFocus onClick={() => closeConfirm(false)}>{t.run.keepDraft}</button></div>
              : <ButtonHint hint={t.run.reinspectHint}><button id="reinspect" ref={reinspectButton} disabled={locked || editOwner || starting || (run.live && busy(view.state)) || !workspace.inspectPath.trim()}
                title={editOwner ? a.inspectBlocked : run.live && busy(view.state) ? t.run.reinspectBlocked : undefined} onClick={confirmedReinspect}>{t.run.reinspect}</button></ButtonHint>}
            {editOwner
              ? <button id="edit-package-return" type="button" onClick={authoring.onReturn}>{a.returnToEdit}</button>
              : confirmEdit
                ? <div className="confirm-row" role="alertdialog" aria-labelledby="confirm-edit-text">
                  <span id="confirm-edit-text">{a.confirmEdit}</span>
                  <button id="edit-discard" type="button" className="danger-text" onClick={() => {setConfirmEdit(false); authoring.onOpen(bound.packagePath);}}>{a.discardEdit}</button>
                  <button id="edit-keep" type="button" autoFocus onClick={() => setConfirmEdit(false)}>{t.run.keepDraft}</button></div>
                : <button id="edit-package" type="button" disabled={authoring.block !== null} title={authoring.block ?? undefined} onClick={confirmedEdit}>{a.editPackage}</button>}
          </div>
          <div className="field"><div className="field-heading"><label htmlFor="package-path">{t.guidance.packageDirectory}</label><HelpTrigger title={t.guidance.packageDirectory} hint={t.run.packageDirectoryHint}>{t.run.reinspectHelp}</HelpTrigger></div>
            <input id="package-path" type="text" value={workspace.inspectPath} disabled={locked} spellCheck={false} placeholder={t.guidance.packagePlaceholder}
              onChange={event => handlers.inspectPath(event.target.value)}/></div>
          {bound.profilesError && <><FaultMessage title={t.run.profilesError} value={bound.profilesError}/>
            {bound.profilesError.category === 'ProfileRejected' && workspace.recovery === null
              ? <p className="muted">{t.run.rejectedHelp} <button id="recover-reinspect" type="button" className="link" disabled={locked || editOwner || starting || (run.live && busy(view.state)) || !workspace.inspectPath.trim()} onClick={confirmedReinspect}>{t.run.recoverReinspect}</button></p>
              : <p className="muted">{t.run.profilesHelp}</p>}</>}
          <div className="two-col">
            <div className="field"><label htmlFor="profile-select">{t.run.savedProfile}</label>
              <Select id="profile-select" value={bound.selectedId ?? ''} disabled={locked} onChange={handlers.selectProfile}
                options={[{value: '', label: t.run.unsavedDraft}, ...bound.profiles.map(profile => ({value: profile.id, label: profile.name}))]}/>
              {selectedProfile && <p className="identity-text">{selectedProfile.id}</p>}
              {!profileBound && <p className="inline-warning">{t.run.staleSchema}</p>}</div>
            <div className="field"><div className="field-heading"><label htmlFor="preset-select">{t.run.preset}</label><HelpTrigger title={t.run.preset} hint={t.run.presetHelp}>{t.run.presetHelp}</HelpTrigger></div>
              <Select id="preset-select" value={bound.preset} disabled={locked} onChange={handlers.newDraft}
                options={[{value: '', label: t.run.defaults}, ...Object.keys(bound.package.profiles).map(key => ({value: key, label: key}))]}/></div>
          </div>
          <div className="field"><label htmlFor="profile-name">{t.run.profileName}</label>
            <input id="profile-name" value={bound.name} disabled={locked} onChange={event => handlers.change(item => ({...item, name: event.target.value, touched: true}))}/></div>
          <div className="button-row">
            <button id="save-profile" className="primary" disabled={locked || !bound.name.trim() || numericErrors || !profileBound} onClick={handlers.saveProfile}>{bound.selectedId ? t.run.updateProfile : t.run.saveProfile}</button>
            <button id="new-profile" disabled={locked} onClick={() => handlers.newDraft()}>{t.run.newDraft}</button>
            <button id="rename-profile" disabled={!bound.selectedId || locked || !bound.name.trim() || !profileBound} onClick={handlers.renameProfile}>{t.run.rename}</button>
            <button id="delete-profile" className="danger-text" disabled={!bound.selectedId || locked} onClick={handlers.deleteProfile}>{t.run.deleteProfile}</button>
          </div>
          <details className="legacy-import"><summary>{t.run.legacyHeading}</summary>
            <p className="field-help">{t.run.legacyHint}<HelpTrigger title={t.run.legacyHeading} hint={t.run.legacyHelpHint}>
              <p>{t.run.legacyHelp}</p><p>{t.bootstrap.converterHelp}</p>
            </HelpTrigger></p>
            <div className="button-row"><ButtonHint hint={t.run.legacyHint}><button id="import-legacy-profiles" disabled={locked} onClick={handlers.importLegacy}>{t.run.legacyImport}</button></ButtonHint></div>
            {legacy && <dl className="run-identity">
              <dt>{t.run.legacyImported}</dt><dd>{legacy.imported.length ? legacy.imported.join(', ') : t.run.legacyNone}</dd>
              <dt>{t.run.legacyUnchanged}</dt><dd>{legacy.unchanged.length ? legacy.unchanged.join(', ') : t.run.legacyNone}</dd></dl>}
            {legacy?.fault && <FaultMessage title={t.run.legacyFault} value={legacy.fault}/>}
          </details>
          <h3>{t.run.options}<HelpTrigger title={t.run.options} hint={t.run.optionsHint}>{t.run.optionsHelp}</HelpTrigger></h3>
          <SchemaForm schema={bound.package.schema} value={bound.draft} onChange={draft => handlers.change(item => editDraft(item, draft))} errors={parsed.errors}/>
          {bound.validation?.draftRevision === bound.draftRevision && <details className="validated"><summary>{t.run.valid}</summary><pre>{JSON.stringify(bound.validation.values, null, 2)}</pre></details>}
          <h3>{t.run.executionMode}</h3>
          <div className="two-col">
            <div className="field"><label htmlFor="lane">{t.run.executionMode}</label>
              <Select id="lane" value={bound.lane} disabled={locked} onChange={lane => handlers.change(item => chooseLane(item, lane))}
                options={[{value: 'controlled', label: t.run.controlled},
                  {value: 'replay', label: t.run.replay},
                  {value: 'native', label: nativeCapability ? t.run.native : t.run.nativeUnavailable, disabled: nativeCapability === null}]}/></div>
            <div className="field"><label htmlFor="scenario">{t.run.scenario}</label>
              <Select id="scenario" value={bound.lane === 'controlled' ? bound.scenario : 'workflow'} disabled={locked || bound.lane !== 'controlled'}
                onChange={scenario => handlers.change(item => ({...item, scenario}))}
                options={[{value: 'workflow', label: t.run.workflow}, {value: 'held-work', label: t.run.heldWork}, {value: 'no-match', label: t.run.noMatch}]}/></div>
          </div>
          {nativeError && <FaultMessage title={t.run.nativeCapabilityError} value={nativeError}/>}
          {native && <div id="native-review">
            <h3>{t.run.nativeHeading}</h3>
            <p className="field-help">{t.run.nativeHelp}</p>
            <dl className="run-identity" id="native-target">
              <dt>{t.run.nativePackage}</dt><dd>{bound.package.package_id} / {selectedProfile && !valuesDirty ? `${selectedProfile.name} · ${selectedProfile.id}` : t.run.profileDraft}</dd>
              <dt>{t.run.nativeApplication}</dt><dd className="mono">{nativeBinding?.configuration.game.path ?? t.common.none}</dd>
              <dt>{t.target.windowTitle}</dt><dd>{nativeBinding?.configuration.window_title ?? t.common.none}</dd>
              <dt>{t.target.route}</dt><dd id="native-route">{nativeInput ? nativeInput.route === 'system' ? t.target.system : t.target.processDirected : t.common.none}</dd>
              <dt>{t.target.focus}</dt><dd id="native-focus">{nativeInput ? nativeInput.focus === 'preserve' ? t.target.preserve : t.target.requireFocused : t.common.none}</dd>
              <dt>{t.target.pointerMode}</dt><dd id="native-pointer">{nativeInput?.pointer_mode === 'core_graphics' ? t.target.coreGraphics : nativeInput?.pointer_mode === 'appkit_background' ? t.target.appkitBackground : t.common.none}</dd>
              <dt>{t.target.clickHold}</dt><dd>{nativeInput ? `${nativeInput.click_hold_ms} ms` : t.common.none}</dd>
              <dt>{t.run.nativeBinding}</dt><dd>{nativeBinding && bound.target.view ? <><code>{nativeBinding.id}</code> · {t.target.revision(bound.target.view.record.revision)}</> : t.common.none}</dd>
            </dl>
            {recipe && <><span className="eyebrow">{t.run.nativeRecipe}</span>
              <dl className="run-identity" id="native-recipe">
                <dt>{t.run.nativeRecipient}</dt><dd id="native-recipient">{recipe.recipient === 'game' ? t.run.nativeRecipientGame
                  : <>{t.run.nativeRecipientLauncher} · {recipe.location.kind === 'bundle' ? t.target.bundle : t.target.executable} · <span className="mono">{recipe.location.path}</span></>}</dd>
                <dt>{t.run.nativeArguments}</dt><dd>{recipe.arguments.length === 0 ? t.run.nativeNoArguments
                  : <ol id="native-arguments" className="native-arguments">{recipe.arguments.map((value, index) => <li key={index}><code>{JSON.stringify(value)}</code></li>)}</ol>}</dd>
                <dt>{t.run.nativeDirectory}</dt><dd id="native-directory" className={recipe.directory === 'refused' ? 'field-error' : undefined}>
                  {recipe.workingDirectory !== null && <><code>{recipe.workingDirectory}</code> · </>}{t.run.nativeDirectories[recipe.directory]}</dd>
              </dl>
              {recipe.recipient === 'launcher' && <p className="field-help">{t.run.nativeForwarding}</p>}</>}
            {nativeCapability && <div className="field"><label htmlFor="native-workflow-seconds">{t.run.nativeWorkflowSeconds}</label>
              <input id="native-workflow-seconds" type="text" inputMode="numeric" value={native.workflowSeconds} disabled={locked} spellCheck={false}
                aria-invalid={native.workflowError} aria-describedby={`native-workflow-help${native.workflowError ? ' native-workflow-error' : ''}`}
                onChange={event => handlers.native.edit('workflowSeconds', event.target.value)}/>
              {native.workflowError && <p className="field-error" id="native-workflow-error">{t.run.nativeWorkflowError(nativeCapability.max_workflow_ms / 1000)}</p>}
              <p className="field-help" id="native-workflow-help">{t.run.nativeWorkflowHelp(nativeCapability.default_limits.workflow_ms / 1000, nativeCapability.max_workflow_ms / 1000)}</p></div>}
            {nativeBudgetRows && <><span className="eyebrow">{t.run.nativeBudgets}<HelpTrigger title={t.run.nativeBudgets} hint={t.run.nativeBudgetHint}>
              <p>{t.run.nativeBudgetHelp}</p><p>{t.run.nativeStartupHelp}</p>
            </HelpTrigger></span>
              <dl className="run-identity" id="native-budgets">{nativeBudgetRows.map(([term, value]) => <Fragment key={term}><dt>{term}</dt><dd>{value === null ? t.common.none : `${value} ms`}</dd></Fragment>)}</dl>
              <dl className="run-identity"><dt>{t.run.nativeDuration}</dt><dd id="native-duration">{nativeDuration === null ? t.common.none : t.run.nativeEnvelope(nativeDuration)}</dd></dl></>}
            {nativeLimitRows && <><span className="eyebrow">{t.run.nativeLimits}</span>
              <dl className="run-identity" id="native-limits">{nativeLimitRows.map(([term, value]) => <Fragment key={term}><dt>{term}</dt><dd>{value}</dd></Fragment>)}</dl></>}
            <div className="field"><label htmlFor="native-operation">{t.run.nativeOperation}</label>
              <input id="native-operation" type="text" value={bound.native.operation} disabled={locked} spellCheck={false} aria-invalid={bound.native.operation !== '' && native.operationError !== null}
                onChange={event => handlers.native.edit('operation', event.target.value)}/>
              {bound.native.operation !== '' && native.operationError && <p className="field-error">{t.run.nativeTextErrors[native.operationError]}</p>}
              <p className="field-help">{t.run.nativeOperationHelp}</p></div>
            <div className="field"><label htmlFor="native-postcondition">{t.run.nativePostcondition}</label>
              <input id="native-postcondition" type="text" value={bound.native.postcondition} disabled={locked} spellCheck={false} aria-invalid={bound.native.postcondition !== '' && native.postconditionError !== null}
                onChange={event => handlers.native.edit('postcondition', event.target.value)}/>
              {bound.native.postcondition !== '' && native.postconditionError && <p className="field-error">{t.run.nativeTextErrors[native.postconditionError]}</p>}
              <p className="field-help">{t.run.nativePostconditionHelp}</p></div>
            <div className="field">
              <label className="checkbox-label"><input id="native-capture-consent" type="checkbox" checked={native.capture} disabled={!approvalOpen}
                onChange={event => handlers.native.approve('capture', event.target.checked)}/>{t.run.nativeCapture}</label>
              <label className="checkbox-label"><input id="native-input-consent" type="checkbox" checked={native.input} disabled={!approvalOpen}
                onChange={event => handlers.native.approve('input', event.target.checked)}/>{t.run.nativeInput}</label>
              <label className="checkbox-label"><input id="native-launch-consent" type="checkbox" checked={native.launch} disabled={!approvalOpen}
                onChange={event => handlers.native.approve('launch', event.target.checked)}/>{t.run.nativeLaunch}</label>
              <p className="field-help" id="native-launch-help">{t.run.nativeLaunchHelp}</p>
              <label className="checkbox-label"><input id="native-recovery-consent" type="checkbox" checked={native.recovery} disabled={!approvalOpen || !native.launch}
                aria-describedby="native-recovery-help" onChange={event => handlers.native.approve('recovery', event.target.checked)}/>{t.run.nativeRecovery}</label>
              <p className="field-help" id="native-recovery-help">{t.run.nativeRecoveryHelp}</p>
              <p className="field-help">{t.run.nativeApprovalHelp}</p></div>
          </div>}
          <div className="field"><div className="field-heading"><label htmlFor="descriptor-path">{t.run.descriptor}</label><HelpTrigger title={t.run.descriptor} hint={t.run.descriptorHint}>{t.run.descriptorHelp(DESCRIPTOR_LIMIT)}</HelpTrigger></div>
            <input id="descriptor-path" type="text" value={bound.descriptorPath} disabled={locked} spellCheck={false} aria-invalid={descriptorError !== null}
              placeholder={t.run.descriptorPlaceholder} onChange={event => handlers.change(item => ({...item, descriptorPath: event.target.value}))}/>
            {descriptorError && <p className="field-error">{descriptorError}</p>}</div>
          {startBlock && <p className="inline-warning" id="start-block">{startBlock}</p>}
        </div>
      </section>
      <TargetPanel state={bound.target} handlers={handlers.target} locked={locked} active={active} pickerBusy={pickerBusy}/>
    </div>
    <ProfileRecovery idPrefix="recovery" label={label} state={workspace.recovery} outcomes={workspace.recoveryOutcomes} locked={locked} handlers={handlers.recovery}/>
  </>;
}

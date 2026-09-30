import {Fragment, useEffect, useRef, useState} from 'react';
import ProfileRecovery from '../components/ProfileRecovery.tsx';
import type {RecoveryHandlers} from '../components/ProfileRecovery.tsx';
import SchemaForm from '../components/SchemaForm.tsx';
import Select from '../components/Select.tsx';
import TargetPanel from '../components/TargetPanel.tsx';
import type {TargetHandlers} from '../components/TargetPanel.tsx';
import ResultPanel, {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {faultSummary, nativeOutcome, text} from '../state.ts';
import type {CheckAssociation} from '../state.ts';
import {DESCRIPTOR_LIMIT, UNSUPPORTED_SOURCE, busy, chooseLane, editDraft, hasWorkspaceEdits} from '../workspace.ts';
import type {Bound, BoundWorkspace, Derived, NativeConsent} from '../workspace.ts';
import {AUTHORING_RECOVERY, recoveryPath} from '../authoring.ts';
import type {PageAuthoring} from './EditPage.tsx';
import type {ControllerView, Fault, Json, NativeIntent, NativeLimits, OcrEnvironment} from '../types.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Immutable facts captured when this frontend submitted the operation; the host view is authoritative.
export type RunSnapshot =
  | {kind: 'run'; run: string; lane: string; packageId: string; profileName: string; profileId: string; scenario: string; descriptorPath: string | null; values: Record<string, Json>; native: NativeIntent | null}
  | {kind: 'check'; run: string; association: CheckAssociation};

export interface RunView {view: ControllerView; live: boolean; olderRevision: number | null}

export interface RunHandlers {
  // Draft-affecting edits; they clear the transient notice. Disclosure toggles do not.
  change: (update: (bound: Bound) => Bound) => void;
  disclose: (run: string | null) => void;
  validate: () => void; saveProfile: () => void; renameProfile: () => void; deleteProfile: () => void;
  newDraft: (preset?: string) => void; selectProfile: (id: string) => void;
  reinspect: () => void; inspectPath: (value: string) => void; importLegacy: () => void; start: () => void; stop: () => void;
  // Native review text edits withdraw consent; consent binds to the request as it is now.
  native: {edit: (field: 'operation' | 'postcondition', value: string) => void; approve: (field: NativeConsent, value: boolean) => void};
  target: TargetHandlers;
  recovery: RecoveryHandlers;
}

interface Props {
  workspace: BoundWorkspace; label: string; derived: Derived; run: RunView; snapshot: RunSnapshot | null;
  locked: boolean; active: boolean; pickerBusy: boolean; starting: boolean; stopping: boolean; closing: boolean;
  savedEnvironment: OcrEnvironment | null; handlers: RunHandlers;
  // Host-issued Native policy; null when this platform/build offers none. `nativeError` is a failed availability read.
  nativeLimits: NativeLimits | null; nativeError: Fault | null;
  // While any Tab owns Edit, Start is refused everywhere; the owner additionally waits to exit before reinspecting.
  authoring: PageAuthoring;
}

export default function RunPage({workspace, label, derived, run, snapshot, locked, active, pickerBusy, starting, stopping, closing, savedEnvironment, handlers, authoring, nativeLimits, nativeError}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const bound = workspace.bound;
  const {parsed, numericErrors, valuesDirty, dirty, bound: profileBound, selectedProfile, startBlock, descriptorError} = derived;
  const [confirmReinspect, setConfirmReinspect] = useState(false);
  const [confirmEdit, setConfirmEdit] = useState(false);
  const a = t.authoring;
  const editOwner = authoring.role === 'owner';
  const editReason = authoring.role !== null && authoring.ownerLabel !== null ? a.startBlocked(authoring.ownerLabel) : null;
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
  const check = view.operation === 'environment_check';
  const primary = view.error ?? (view.result?.primary ? fault(view.result.primary) : null);
  const privatePrimary = !check && (snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane)) !== 'controlled';
  const selectedDescriptor = bound.descriptorPath.trim() || null;
  const canStart = !locked && !active && !pickerBusy && authoring.role === null && !numericErrors && profileBound && startBlock === null && descriptorError === null;
  const stopAvailable = run.live && view.run !== null && busy(view.state) && view.state !== 'stopping' && !stopping && !starting && !closing;
  const executionCaption = starting ? t.run.submitted : run.live && busy(view.state) ? t.run.owned(view.run) : view.state === 'terminal' ? t.run.settled(view.run) : t.run.noOperations;
  const heading = selectedProfile && !valuesDirty ? selectedProfile.name : bound.name || t.run.untitled;
  const legacy = workspace.legacyImport;
  const native = derived.native;
  const nativeBinding = native?.binding ?? null;
  const nativeInput = nativeBinding?.configuration.input ?? null;
  const recipe = native?.recipe ?? null;
  // Launch recipe validity is conditional on host-confirmed absence, not a gate on attaching.
  const approvalOpen = native !== null && (native.block === null || native.block === 'nativeApproval') && !locked;
  const outcome = nativeOutcome(run.view);
  const nativeLimitRows = nativeLimits && [
    [t.run.nativeDuration, `${nativeLimits.duration_ms} ms`], [t.run.nativeFrames, String(nativeLimits.max_frames)],
    [t.run.nativeWait, `${nativeLimits.wait_ms} ms`], [t.run.nativeInterval, `${nativeLimits.interval_ms} ms`],
    [t.run.nativeActions, String(nativeLimits.max_actions)], [t.run.nativeCleanup, `${nativeLimits.cleanup_ms} ms`],
    [t.run.nativeContainment, `${nativeLimits.containment_ms} ms`],
  ];
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.run.heading(label)}</span><h1>{heading}</h1>
      <p>{t.run.introduction}</p></div>
      <div className="actions">
        <button id="validate" disabled={locked || numericErrors || !profileBound} onClick={handlers.validate}>{t.run.validate}</button>
        <button id="start" className="primary" disabled={!canStart} onClick={handlers.start}>{t.run.start}</button>
      </div>
    </div>
    <div id="error">
      {workspace.error && <FaultMessage title={t.run.actionFailed} value={workspace.error}/>}
      {workspace.error?.category === AUTHORING_RECOVERY && <div className="button-row">
        <button id="workspace-recover" type="button" disabled={locked} onClick={() => authoring.onRecover(recoveryPath(workspace.error!, bound.packagePath))}>{a.recover}</button>
        <span className="muted">{a.recoverHelp}</span></div>}
      {workspace.sourceError && <FaultMessage title={workspace.sourceError.category === UNSUPPORTED_SOURCE ? t.guidance.unsupportedHeading : t.guidance.unavailableHeading} value={workspace.sourceError}/>}
      {primary && (privatePrimary || check
        ? <section className="fault" role="alert"><strong>{check ? t.run.checkError : t.run.runError} · {faultSummary(primary, !privatePrimary)}</strong>
            <p>{t.run.diagnosticHelp}</p></section>
        : <FaultMessage title={t.run.runError} value={primary}/>)}
      {outcome?.failure && <p className="inline-warning" id="native-failure">{outcome.cause ? t.run.nativeCauses[outcome.cause] : t.run.nativeFailures[outcome.failure]}</p>}
      {outcome?.launch && <p className="inline-warning" id="native-launch-outcome">{t.run.nativeLaunchOutcomes[outcome.launch]}</p>}
    </div>
    <div className="operation-status" role="status">{renderMessage(locale, workspace.busy ?? workspace.notice) || (startBlock ?? '')}</div>
    {editReason && <p id="start-authoring-block" className="inline-warning">{editReason}
      {editOwner && <button id="run-return-to-edit" type="button" onClick={authoring.onReturn}>{a.returnToEdit}</button>}</p>}
    <section className="panel summary-panel" aria-label={t.run.summary}>
      <div className="summary-item"><span className="eyebrow">{t.common.execution}</span>
        <div className="state-line"><span className={`dot phase-${phase}`} aria-hidden="true"/><span id="state" className={`phase phase-${phase}`}>{t.phase(phase)}</span></div>
        <p className="summary-caption">{executionCaption}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.common.profile}</span>
        <div className="summary-value">{selectedProfile ? selectedProfile.name : bound.name || t.run.untitled}</div>
        <p className={`summary-caption save-status ${dirty ? 'unsaved' : ''}`}>{selectedProfile ? dirty ? t.run.profileChanged : t.run.profileSaved : t.run.profileDraft}</p></div>
      <div className="summary-item"><span className="eyebrow">{t.run.inputRoute}</span>
        <div className="summary-value">{t.lane(bound.lane)}</div>
        <p className="summary-caption">{bound.lane === 'replay' ? t.run.replaySummary : bound.lane === 'native' ? t.run.nativeSummary : t.run.controlledSummary}</p></div>
    </section>
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
              : <button id="reinspect" ref={reinspectButton} disabled={locked || editOwner || starting || (run.live && busy(view.state)) || !workspace.inspectPath.trim()}
                title={editOwner ? a.inspectBlocked : run.live && busy(view.state) ? t.run.reinspectBlocked : undefined} onClick={confirmedReinspect}>{t.run.reinspect}</button>}
            {editOwner
              ? <button id="edit-package-return" type="button" onClick={authoring.onReturn}>{a.returnToEdit}</button>
              : confirmEdit
                ? <div className="confirm-row" role="alertdialog" aria-labelledby="confirm-edit-text">
                  <span id="confirm-edit-text">{a.confirmEdit}</span>
                  <button id="edit-discard" type="button" className="danger-text" onClick={() => {setConfirmEdit(false); authoring.onOpen(bound.packagePath);}}>{a.discardEdit}</button>
                  <button id="edit-keep" type="button" autoFocus onClick={() => setConfirmEdit(false)}>{t.run.keepDraft}</button></div>
                : <button id="edit-package" type="button" disabled={authoring.block !== null} title={authoring.block ?? undefined} onClick={confirmedEdit}>{a.editPackage}</button>}
          </div>
          <div className="field"><label htmlFor="package-path">{t.guidance.packageDirectory}</label>
            <input id="package-path" type="text" value={workspace.inspectPath} disabled={locked} spellCheck={false} placeholder={t.guidance.packagePlaceholder}
              onChange={event => handlers.inspectPath(event.target.value)}/>
            <p className="field-help">{t.run.reinspectHelp}</p></div>
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
            <div className="field"><label htmlFor="preset-select">{t.run.preset}</label>
              <Select id="preset-select" value={bound.preset} disabled={locked} onChange={handlers.newDraft}
                options={[{value: '', label: t.run.defaults}, ...Object.keys(bound.package.profiles).map(key => ({value: key, label: key}))]}/>
              <p className="field-help">{t.run.presetHelp}</p></div>
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
            <p className="field-help">{t.run.legacyHelp}</p>
            <div className="button-row"><button id="import-legacy-profiles" disabled={locked} onClick={handlers.importLegacy}>{t.run.legacyImport}</button></div>
            {legacy && <dl className="run-identity">
              <dt>{t.run.legacyImported}</dt><dd>{legacy.imported.length ? legacy.imported.join(', ') : t.run.legacyNone}</dd>
              <dt>{t.run.legacyUnchanged}</dt><dd>{legacy.unchanged.length ? legacy.unchanged.join(', ') : t.run.legacyNone}</dd></dl>}
            {legacy?.fault && <FaultMessage title={t.run.legacyFault} value={legacy.fault}/>}
          </details>
          <h3>{t.run.options}</h3>
          <p className="field-help">{t.run.optionsHelp}</p>
          <SchemaForm schema={bound.package.schema} value={bound.draft} onChange={draft => handlers.change(item => editDraft(item, draft))} errors={parsed.errors}/>
          {bound.validation && <details className="validated"><summary>{t.run.valid}</summary><pre>{JSON.stringify(bound.validation, null, 2)}</pre></details>}
          <h3>{t.run.route}</h3>
          <div className="two-col">
            <div className="field"><label htmlFor="lane">{t.run.lane}</label>
              <Select id="lane" value={bound.lane} disabled={locked} onChange={lane => handlers.change(item => chooseLane(item, lane))}
                options={[{value: 'controlled', label: t.run.controlled},
                  {value: 'replay', label: t.run.replay},
                  {value: 'native', label: nativeLimits ? t.run.native : t.run.nativeUnavailable, disabled: nativeLimits === null}]}/></div>
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
              <p className="field-help">{t.run.nativeApprovalHelp}</p></div>
          </div>}
          <div className="field"><label htmlFor="descriptor-path">{t.run.descriptor}</label>
            <input id="descriptor-path" type="text" value={bound.descriptorPath} disabled={locked} spellCheck={false} aria-invalid={descriptorError !== null}
              placeholder={t.run.descriptorPlaceholder} onChange={event => handlers.change(item => ({...item, descriptorPath: event.target.value}))}/>
            {descriptorError && <p className="field-error">{descriptorError}</p>}
            <p className="field-help">{t.run.descriptorHelp(DESCRIPTOR_LIMIT)}</p></div>
          <p className="muted">{t.run.startUses(selectedProfile && !valuesDirty ? selectedProfile.name : null, bound.lane === 'replay', savedEnvironment?.profile ?? null, selectedDescriptor)}</p>
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
            {view.native_preparation && <><dt>{t.run.nativeStage}</dt><dd id="native-stage">{t.run.nativePhases[view.native_preparation.phase]}</dd>
              <dt>{t.run.nativeLaunchRequest}</dt><dd id="native-launch">{t.run.launchDispositions[view.native_preparation.launch]}</dd></>}
            {snapshot?.kind === 'run' && snapshot.run === view.run && <><dt>{t.run.capturedProfile}</dt><dd>{snapshot.profileName || t.run.untitled} · {snapshot.profileId}</dd><dt>{t.run.packageScenario}</dt><dd>{snapshot.packageId} / {snapshot.scenario}</dd>
              {snapshot.lane === 'replay' && <><dt>{t.common.descriptor}</dt><dd>{snapshot.descriptorPath}</dd></>}
              {snapshot.native && <><dt>{t.run.nativeOperation}</dt><dd>{snapshot.native.operation}</dd>
                <dt>{t.run.nativePostcondition}</dt><dd>{snapshot.native.visible_postcondition}</dd>
                <dt>{t.run.nativeBinding}</dt><dd><code>{snapshot.native.target_binding_id}</code> · {t.target.revision(snapshot.native.target_revision)}</dd>
                <dt>{t.run.nativeLaunchApproval}</dt><dd>{snapshot.native.launch_approved ? t.run.nativeLaunchApproved : t.run.nativeLaunchNotApproved}</dd></>}</>}
            {snapshot?.kind === 'check' && snapshot.run === view.run && <><dt>{t.run.checkedProfile}</dt><dd>{snapshot.association.environment?.profile ?? t.common.unconfigured}</dd>
              <dt>{t.common.descriptor}</dt><dd>{snapshot.association.descriptorPath ?? t.run.noInitialization}</dd></>}
          </dl>
          {snapshot?.kind === 'run' && snapshot.run === view.run && <details><summary>{t.run.capturedOptions}</summary><pre>{JSON.stringify(snapshot.values, null, 2)}</pre></details>}
          {snapshot?.kind === 'run' && snapshot.run === view.run && snapshot.native && <details><summary>{t.run.capturedLimits}</summary><pre>{JSON.stringify(snapshot.native.limits, null, 2)}</pre></details>}
          {snapshot?.kind === 'check' && snapshot.run === view.run && <details><summary>{t.run.capturedEnvironment}</summary><pre>{JSON.stringify(snapshot.association.environment, null, 2)}</pre></details>}
          <h3>{t.run.milestones}</h3><ol className="progress-list">{view.progress.map((event, index) => <li key={`${view.run}-${index}`}>
            <details><summary>{String(event.event ?? t.run.milestone)}{text(event.stage) ? ` · ${event.stage}` : ''}{typeof event.at_us === 'number' ? ` · ${(event.at_us / 1000).toFixed(1)} ms` : ''}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>
          </li>)}</ol>{view.progress.length === 0 && <p className="muted">{t.run.noMilestones}</p>}
          <ResultPanel view={view} disclosed={view.run !== null && bound.disclosedRun === view.run} onDisclose={next => handlers.disclose(next ? view.run : null)}/>
        </div>
      </section>
    </div>
    <ProfileRecovery idPrefix="recovery" label={label} state={workspace.recovery} outcomes={workspace.recoveryOutcomes} locked={locked} handlers={handlers.recovery}/>
    <TargetPanel state={bound.target} handlers={handlers.target} locked={locked} active={active} pickerBusy={pickerBusy}/>
  </>;
}

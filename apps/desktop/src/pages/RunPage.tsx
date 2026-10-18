import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import ProfileRecovery from '../components/ProfileRecovery.tsx';
import type {RecoveryHandlers} from '../components/ProfileRecovery.tsx';
import ExecutionFields from '../components/ExecutionFields.tsx';
import Select from '../components/Select.tsx';
import TargetPanel from '../components/TargetPanel.tsx';
import type {TargetHandlers} from '../components/TargetPanel.tsx';
import NativeReview from '../components/NativeReview.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import {ButtonHint, HelpTrigger} from '../components/ContextualHelp.tsx';
import type {RunView} from './ExecutionPage.tsx';
import {UNSUPPORTED_SOURCE, busy, hasWorkspaceEdits} from '../workspace.ts';
import type {ReleaseFacts} from '../distribution.ts';
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
  saveProfile: () => void; renameProfile: () => void; deleteProfile: () => void;
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
  // The App-owned installed-package launcher, placed in PACKAGE SOURCE; admission and its owner stay with App.
  packageAction: ReactNode;
  // Listed verified facts of the saved installed release, if the current listing has them; display only.
  release: ReleaseFacts | null;
}

export default function RunPage({workspace, label, derived, run, locked, active, pickerBusy, starting, environment, handlers, authoring, nativeCapability, nativeError, packageAction, release}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const bound = workspace.bound;
  const {numericErrors, valuesDirty, dirty, bound: profileBound, selectedProfile, startBlock, descriptorError} = derived;
  const [confirmReinspect, setConfirmReinspect] = useState(false);
  const [confirmEdit, setConfirmEdit] = useState(false);
  const a = t.authoring;
  const editOwner = authoring.role === 'owner';
  // Installed releases reinspect by saved identity and have no Edit entry point.
  const installed = workspace.savedPackage?.source.kind === 'installed' ? workspace.savedPackage.source : null;
  const reinspectable = installed !== null || workspace.inspectPath.trim() !== '';
  // A changed path makes Reinspect inspect that directory instead of the installed release (see App); its
  // attribution stays visible while the one editable field remains in the collapsed details.
  const pendingPath = installed !== null && workspace.inspectPath.trim() !== '' && workspace.inspectPath.trim() !== bound.packagePath
    ? workspace.inspectPath.trim() : null;
  const reinspectButton = useRef<HTMLButtonElement>(null);
  const confirmRow = useRef<HTMLDivElement>(null);
  // Set only when the confirmation row closes while it owns focus. Focus returns to Reinspect; once the submitted
  // command locks that button (and a fresh revision remounts this page), the stable Run page tab takes it.
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
  const startReason = authoring.role !== null ? a.startBlocked(authoring.ownerLabel ?? label) : startBlock;
  const executionCaption = starting ? t.run.submitted : run.live && busy(view.state) ? t.run.owned(view.run) : view.state === 'terminal' ? t.run.settled(view.run) : t.run.noOperations;
  const heading = selectedProfile && !valuesDirty ? selectedProfile.name : bound.name || t.run.untitled;
  const legacy = workspace.legacyImport;
  const native = derived.native;
  // A directory source relocates through this path. An installed source reinspects by its saved identity, so the
  // path (which can still inspect another directory instead) stays among its collapsed details.
  const pathHeading = <div className="field-heading"><label htmlFor="package-path">{t.guidance.packageDirectory}</label><HelpTrigger title={t.guidance.packageDirectory} hint={t.run.packageDirectoryHint}>{t.run.reinspectHelp}</HelpTrigger></div>;
  const pathInput = <input id="package-path" type="text" value={workspace.inspectPath} disabled={locked} spellCheck={false} placeholder={t.guidance.packagePlaceholder}
    onChange={event => handlers.inspectPath(event.target.value)}/>;
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.run.heading(label)}</span><h1>{heading}<HelpTrigger title={t.run.heading(label)} hint={t.run.introductionHint}>{t.run.introduction}</HelpTrigger></h1></div></div>
    <div id="error">
      {workspace.error && <FaultMessage title={t.run.actionFailed} value={workspace.error}/>}
      {workspace.error?.category === AUTHORING_RECOVERY && <div className="button-row">
        <button id="workspace-recover" type="button" disabled={locked} onClick={() => authoring.onRecover(recoveryPath(workspace.error!, bound.packagePath))}>{a.recover}</button>
        <span className="muted">{a.recoverHelp}</span></div>}
      {workspace.sourceError && <FaultMessage title={workspace.sourceError.category === UNSUPPORTED_SOURCE ? t.guidance.unsupportedHeading
        : installed ? t.distribution.installedUnavailableHeading : t.guidance.unavailableHeading} value={workspace.sourceError}/>}
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
        <ButtonHint hint={startReason ?? t.run.startHint}><button id="start" className="primary" disabled={!canStart}
          aria-describedby={['run-start-uses', authoring.role !== null ? 'start-edit-block' : startBlock ? 'start-block' : null].filter(Boolean).join(' ')}
          onClick={handlers.start}>{t.run.start}</button></ButtonHint>
      </div>
    </section>
    <p id="run-start-uses" className="muted">{t.run.startUses(selectedProfile && !valuesDirty ? selectedProfile.name : null, bound.lane === 'replay', environment?.profile ?? null, bound.descriptorPath.trim() || null)}</p>
    {authoring.role !== null && <p id="start-edit-block" className="muted">{startReason}</p>}
    <div className="run-grid">
      <section className="panel" aria-labelledby="config-heading">
        <div className="panel-heading"><h2 id="config-heading">{t.run.configuration}</h2><span className="tag">{dirty ? t.common.unsavedChanges : t.run.savedValues}</span></div>
        <div className="panel-body">
          <section className="package-source" aria-labelledby="package-source-heading">
            <div className="package-source-heading"><div className="package-source-title">
              <span id="package-source-heading" className="eyebrow">{t.run.sourceEyebrow}</span>
              {installed === null && <span className="tag package-source-kind">{t.run.directoryPackage}</span>}
            </div>
              <div className="package-source-controls">
                <div className="package-source-reinspect">{!confirmReinspect && <ButtonHint hint={t.run.reinspectHint}><button id="reinspect" ref={reinspectButton} disabled={locked || editOwner || starting || (run.live && busy(view.state)) || !reinspectable}
                  aria-describedby={pendingPath !== null ? 'package-source-pending' : undefined}
                  title={editOwner ? a.inspectBlocked : run.live && busy(view.state) ? t.run.reinspectBlocked : undefined} onClick={confirmedReinspect}>{t.run.reinspect}</button></ButtonHint>}</div>
                <div className="package-source-change">{packageAction}</div>
              </div></div>
            <div className="package-source-body">
              <div className="package-source-facts">
                <div className="package-source-identity">
                  {installed
                    ? <strong>{release ? t.packageLibrary.currentInstalled(release.name, release.release_version) : t.distribution.installedSource(bound.package.package_id)}</strong>
                    : <strong>{bound.package.package_id}</strong>}
                  <span className="tag">{t.run.inspected(workspace.revision)}</span></div>
                {pendingPath !== null && <p id="package-source-pending" className="inline-warning">{t.run.pendingSource(pendingPath)}</p>}
                {installed === null && <div className="field">{pathHeading}</div>}
              </div>
              {(editOwner || (installed === null && !confirmEdit)) && <div className="package-source-edit">
                {editOwner
                  ? <button id="edit-package-return" type="button" onClick={authoring.onReturn}>{a.returnToEdit}</button>
                  : <button id="edit-package" type="button" disabled={authoring.block !== null} title={authoring.block ?? undefined} onClick={confirmedEdit}>{a.editPackage}</button>}
              </div>}
              {installed === null && <div className="package-source-path field">{pathInput}</div>}
            </div>
            {(confirmReinspect || confirmEdit) && <div className="package-source-actions">
              {confirmReinspect && <div ref={confirmRow} className="confirm-row" role="alertdialog" aria-labelledby="confirm-reinspect-text">
                  <span id="confirm-reinspect-text">{t.run.confirmReinspect}</span>
                  <button type="button" className="danger-text" onClick={() => closeConfirm(true)}>{t.run.discardReinspect}</button>
                  <button type="button" autoFocus onClick={() => closeConfirm(false)}>{t.run.keepDraft}</button></div>}
              {confirmEdit && <div className="confirm-row" role="alertdialog" aria-labelledby="confirm-edit-text">
                <span id="confirm-edit-text">{a.confirmEdit}</span>
                <button id="edit-discard" type="button" className="danger-text" onClick={() => {setConfirmEdit(false); authoring.onOpen(bound.packagePath);}}>{a.discardEdit}</button>
                <button id="edit-keep" type="button" autoFocus onClick={() => setConfirmEdit(false)}>{t.run.keepDraft}</button></div>}
            </div>}
            <details className="package-source-details"><summary>{t.run.sourceDetails}</summary>
              <dl className="run-identity">
                <dt>{t.distribution.packageId}</dt><dd className="mono">{bound.package.package_id}</dd>
                <dt>{t.distribution.runtime}</dt><dd className="mono">{bound.package.runtime}</dd>
                <dt>{t.run.sourceLocation}</dt><dd className="mono">{bound.packagePath}</dd>
                {installed && <><dt>{t.distribution.artifact}</dt><dd id="run-installed-source" className="mono">{installed.artifact_id}</dd></>}</dl>
              {installed !== null && <div className="field">{pathHeading}{pathInput}</div>}
            </details>
          </section>
          {bound.profilesError && <><FaultMessage title={t.run.profilesError} value={bound.profilesError}/>
            {bound.profilesError.category === 'ProfileRejected' && workspace.recovery === null
              ? <p className="muted">{t.run.rejectedHelp} <button id="recover-reinspect" type="button" className="link" disabled={locked || editOwner || starting || (run.live && busy(view.state)) || !reinspectable} onClick={confirmedReinspect}>{t.run.recoverReinspect}</button></p>
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
          <ExecutionFields bound={bound} facts={derived} locked={locked} nativeCapability={nativeCapability} change={handlers.change}>
            {nativeError && <FaultMessage title={t.run.nativeCapabilityError} value={nativeError}/>}
            {native && <NativeReview bound={bound} native={native} selectedProfile={selectedProfile} valuesDirty={valuesDirty}
              locked={locked} nativeCapability={nativeCapability} environment={environment} handlers={handlers}/>}
          </ExecutionFields>
          {startBlock && <p className="inline-warning" id="start-block">{startBlock}</p>}
        </div>
      </section>
      <TargetPanel state={bound.target} handlers={handlers.target} locked={locked} active={active} pickerBusy={pickerBusy}/>
    </div>
    <ProfileRecovery idPrefix="recovery" label={label} state={workspace.recovery} outcomes={workspace.recoveryOutcomes} locked={locked} handlers={handlers.recovery}/>
  </>;
}

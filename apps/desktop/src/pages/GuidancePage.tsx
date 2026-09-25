import {useEffect, useRef, useState} from 'react';
import ProfileRecovery from '../components/ProfileRecovery.tsx';
import type {RecoveryHandlers} from '../components/ProfileRecovery.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import {AUTHORING_RECOVERY, recoveryPath} from '../authoring.ts';
import type {PageAuthoring} from './EditPage.tsx';
import {packageDestination} from '../state.ts';
import {UNSUPPORTED_SOURCE, hasWorkspaceEdits} from '../workspace.ts';
import type {Workspace} from '../workspace.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Who holds the single operation slot while this Tab is shown: the Application (a package-less check) or another Tab.
export type ActiveOwner = 'application' | 'other' | null;

interface Props {
  workspace: Workspace; label: string;
  // True while another command or the closing application refuses new inspection; the path stays editable.
  locked: boolean; lockReason: string | null;
  onPath: (value: string) => void; onInspect: () => void;
  // Stop for a foreign owner lives in the strip; the page only names the owner's scope.
  activeOwner: ActiveOwner;
  recovery: RecoveryHandlers;
  authoring: PageAuthoring;
  packagesRoot: string;
}

// Main content for a Tab without a usable package. Honest states: no saved package (Edit guidance), a saved custom
// archive this desktop cannot inspect, a saved directory whose inspection failed, a saved reference that is simply not
// bound, an inspected directory whose durable binding failed, or a same-package relocation candidate whose rejected
// profiles block binding. Inspect and Edit Open/Create are separate real actions; neither runs package code or
// invents inventory. The saved reference is shown so the operator knows which source to repair; showing it grants
// nothing, and a recovery candidate is not a selection until an explicit binding retry succeeds.
export default function GuidancePage({workspace, label, locked, lockReason, onPath, onInspect, activeOwner, recovery, authoring, packagesRoot}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const g = t.guidance;
  const source = workspace.sourceError;
  const saved = workspace.savedPackage;
  const unsupported = source !== null && source.category === UNSUPPORTED_SOURCE;
  const context = workspace.recovery?.view ?? null;
  const candidate = context !== null && context.relocation;
  const pending = context !== null && context.binding_required;
  // The page names the operator's last inspection while a candidate or failed binding is pending; otherwise the saved
  // source's own state. A saved-source fault is a distinct fact and is listed as such in the panel body.
  const [eyebrow, heading, intro] = candidate ? [t.recovery.eyebrow, t.recovery.relocationHeading, t.recovery.relocationHelp]
    : pending ? [g.bindingEyebrow, g.bindingHeading, g.bindingHelp]
    : source === null ? saved === null ? [g.eyebrow, g.heading, g.intro] : [g.savedEyebrow, g.savedHeading, g.savedHelp]
    : unsupported ? [g.unsupportedEyebrow, g.unsupportedHeading, g.unsupportedHelp] : [g.unavailableEyebrow, g.unavailableHeading, g.unavailableHelp];
  const sourceHeading = unsupported ? g.unsupportedHeading : g.unavailableHeading;
  const path = workspace.inspectPath;
  const a = t.authoring;
  const destination = packageDestination(packagesRoot, workspace.editPackageId.trim());
  const owner = authoring.role === 'owner';
  // The owner must leave Edit first: its inspection is the handoff back to Run.
  const inspectReason = owner ? a.inspectBlocked : lockReason;
  const [confirmInspect, setConfirmInspect] = useState(false);
  const inspectButton = useRef<HTMLButtonElement>(null);
  const confirmRow = useRef<HTMLDivElement>(null);
  // Set only when the confirmation closes while it owns focus; Inspect takes it back, or the stable page tab once the
  // submitted command has locked the button.
  const refocus = useRef(false);
  useEffect(() => {
    if (confirmInspect || !refocus.current) return;
    refocus.current = false;
    const button = inspectButton.current;
    (button && !button.disabled ? button : document.getElementById('page-run'))?.focus();
  }, [confirmInspect]);
  function closeConfirm(inspect: boolean) {
    refocus.current = confirmRow.current?.contains(document.activeElement) ?? false;
    setConfirmInspect(false);
    if (inspect) onInspect();
  }
  // A new inspection replaces the recovery context and its draft; the button and Enter share the unsaved-edits check.
  function confirmedInspect() {
    if (locked || owner || !path.trim()) return;
    if (hasWorkspaceEdits(workspace, undefined)) setConfirmInspect(true); else onInspect();
  }
  return <>
    <div className="page-heading"><div><span className="eyebrow">{g.scope(label)}</span><h1>{heading}</h1>
      <p>{intro}</p></div></div>
    <div className="operation-status" role="status">{renderMessage(locale, workspace.busy ?? workspace.notice)}</div>
    <section className="panel guidance-panel" aria-labelledby="guidance-heading">
      <div className="panel-heading"><div><span className="eyebrow">{eyebrow}</span><h2 id="guidance-heading">{heading}</h2></div>
        <span className="tag">{workspace.internalName}</span></div>
      <div className="panel-body">
        {source !== null && <FaultMessage title={sourceHeading} value={source}/>}
        {source === null && saved !== null && <p id="saved-unbound" className="muted">{g.savedUnbound}</p>}
        {source === null && saved === null && <p className="muted">{context !== null ? g.noSavedBinding : g.where}</p>}
        {saved !== null && <dl className="run-identity"><dt>{saved.source.kind === 'directory' ? t.reopen.directory(saved.package_id) : t.reopen.archive(saved.package_id)}</dt><dd className="mono">{saved.source.path}</dd></dl>}
        {activeOwner !== null && <p className="authority-note">{activeOwner === 'application' ? g.applicationStopHelp : g.stopHelp}</p>}
      </div>
    </section>
    {owner && <section id="guidance-edit-owner" className="panel open-form" aria-labelledby="guidance-edit-owner-heading"><div className="panel-body">
      <h2 id="guidance-edit-owner-heading">{a.ownerHeading}</h2>
      <p className="muted">{authoring.loaded ? a.ownerHelp : a.resumeHelp}</p>
      <div className="button-row"><button id="guidance-return-to-edit" type="button" className="primary" onClick={authoring.onReturn}>{a.returnToEdit}</button></div>
    </div></section>}
    {!owner && <section id="edit-form" className="panel open-form" aria-labelledby="edit-heading"><div className="panel-body">
      <h2 id="edit-heading">{a.openHeading}</h2>
      <p className="muted">{a.openHelp}</p>
      <div className="open-row"><div className="field"><label htmlFor="authoring-package-id">{a.packageId}</label>
        <input id="authoring-package-id" type="text" value={workspace.editPackageId} disabled={workspace.busy !== null} spellCheck={false}
          onChange={event => authoring.onPackageId(event.target.value)}/></div>
        <button id="authoring-create" type="button" className="primary" disabled={authoring.block !== null || destination === null} onClick={authoring.onCreate}>{a.create}</button>
      </div>
      <p className="field-help">{a.packageIdHelp}</p>
      <dl className="run-identity"><dt>{a.packagesRoot}</dt><dd className="mono">{packagesRoot}</dd>
        {destination && <><dt>{a.createDestination}</dt><dd id="authoring-destination" className="mono">{destination}</dd></>}</dl>
      <div id="authoring-block" className="muted" role="status">{authoring.block ?? ''}</div>
      <hr/>
      <div className="open-row"><div className="field"><label htmlFor="authoring-path">{a.packageDirectory}</label>
        <input id="authoring-path" type="text" value={workspace.editPath} disabled={workspace.busy !== null} placeholder={a.packagePlaceholder} spellCheck={false}
          onChange={event => authoring.onPath(event.target.value)}/></div>
        <button id="authoring-open" type="button" disabled={authoring.block !== null || !workspace.editPath.trim()} onClick={() => authoring.onOpen(workspace.editPath.trim())}>{a.open}</button>
      </div>
    </div></section>}
    <ProfileRecovery idPrefix="recovery" label={label} state={workspace.recovery} outcomes={workspace.recoveryOutcomes} locked={locked} handlers={recovery}/>
    <section className="panel open-form" aria-labelledby="inspect-heading">
      <div className="panel-body">
        <h2 id="inspect-heading">{g.inspectHeading}</h2>
        <p className="muted">{g.inspectHelp}</p>
        <div className="open-row"><div className="field"><label htmlFor="package-path">{g.packageDirectory}</label>
          <input id="package-path" type="text" value={path} disabled={workspace.busy !== null} placeholder={g.packagePlaceholder} spellCheck={false}
            onChange={event => onPath(event.target.value)} onKeyDown={event => {if (event.key === 'Enter') confirmedInspect();}}/></div>
          <button id="inspect" ref={inspectButton} className="primary" disabled={locked || owner || !path.trim()} title={inspectReason ?? undefined} onClick={confirmedInspect}>{g.inspect}</button></div>
        {confirmInspect && <div ref={confirmRow} className="confirm-row" role="alertdialog" aria-labelledby="confirm-inspect-text">
          <span id="confirm-inspect-text">{g.confirmInspect}</span>
          <button id="inspect-discard" type="button" className="danger-text" onClick={() => closeConfirm(true)}>{g.discardInspect}</button>
          <button id="inspect-keep" type="button" autoFocus onClick={() => closeConfirm(false)}>{t.run.keepDraft}</button></div>}
        <div className="operation-status" role="status">{inspectReason ?? ''}</div>
        {workspace.error && <><FaultMessage title={g.actionFailed} value={workspace.error}/>
          {workspace.error.category === AUTHORING_RECOVERY && <div className="button-row">
            <button id="workspace-recover" type="button" disabled={locked} onClick={() => authoring.onRecover(recoveryPath(workspace.error!, workspace.editPath.trim() || path.trim()))}>{a.recover}</button>
            <span className="muted">{a.recoverHelp}</span></div>}
          {workspace.recoveryOutcomes.length === 0 && !pending && <p className="muted">{g.inspectRecovery}</p>}</>}
      </div>
    </section>
  </>;
}

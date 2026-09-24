import ProfileRecovery from '../components/ProfileRecovery.tsx';
import type {RecoveryHandlers} from '../components/ProfileRecovery.tsx';
import {FaultMessage} from '../components/ResultPanel.tsx';
import {UNSUPPORTED_SOURCE} from '../workspace.ts';
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
}

// Main content for a Tab without a usable package. Honest states: no saved package (Edit guidance), a saved custom
// archive this desktop cannot inspect, a saved directory whose inspection failed, or a same-package relocation
// candidate whose rejected profiles block binding. Inspect is the one real action and is separate from creation; it
// never invents inventory. The saved reference is shown so the operator knows which source to repair; showing it
// grants nothing, and a recovery candidate is not a selection until an explicit binding retry succeeds.
export default function GuidancePage({workspace, label, locked, lockReason, onPath, onInspect, activeOwner, recovery}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const g = t.guidance;
  const source = workspace.sourceError;
  const saved = workspace.savedPackage;
  const unsupported = source !== null && source.category === UNSUPPORTED_SOURCE;
  const candidate = workspace.recovery !== null && workspace.recovery.view.relocation;
  const eyebrow = candidate ? t.recovery.eyebrow : source === null ? g.eyebrow : unsupported ? g.unsupportedEyebrow : g.unavailableEyebrow;
  const heading = candidate ? t.recovery.relocationHeading : source === null ? g.heading : unsupported ? g.unsupportedHeading : g.unavailableHeading;
  const sourceHeading = unsupported ? g.unsupportedHeading : g.unavailableHeading;
  const path = workspace.inspectPath;
  return <>
    <div className="page-heading"><div><span className="eyebrow">{g.scope(label)}</span><h1>{heading}</h1>
      <p>{candidate ? t.recovery.relocationHelp : source === null ? g.intro : unsupported ? g.unsupportedHelp : g.unavailableHelp}</p></div></div>
    <div className="operation-status" role="status">{renderMessage(locale, workspace.busy ?? workspace.notice)}</div>
    <section className="panel guidance-panel" aria-labelledby="guidance-heading">
      <div className="panel-heading"><div><span className="eyebrow">{eyebrow}</span><h2 id="guidance-heading">{heading}</h2></div>
        <span className="tag">{workspace.internalName}</span></div>
      <div className="panel-body">
        {source === null && <p className="muted">{g.where}</p>}
        {source !== null && <FaultMessage title={sourceHeading} value={source}/>}
        {saved !== null && <dl className="run-identity"><dt>{saved.source.kind === 'directory' ? t.reopen.directory(saved.package_id) : t.reopen.archive(saved.package_id)}</dt><dd className="mono">{saved.source.path}</dd></dl>}
        {activeOwner !== null && <p className="authority-note">{activeOwner === 'application' ? g.applicationStopHelp : g.stopHelp}</p>}
      </div>
    </section>
    <ProfileRecovery idPrefix="recovery" label={label} state={workspace.recovery} outcomes={workspace.recoveryOutcomes} locked={locked} handlers={recovery}/>
    <section className="panel open-form" aria-labelledby="inspect-heading">
      <div className="panel-body">
        <h2 id="inspect-heading">{g.inspectHeading}</h2>
        <p className="muted">{g.inspectHelp}</p>
        <div className="open-row"><div className="field"><label htmlFor="package-path">{g.packageDirectory}</label>
          <input id="package-path" type="text" value={path} disabled={workspace.busy !== null} placeholder={g.packagePlaceholder} spellCheck={false}
            onChange={event => onPath(event.target.value)} onKeyDown={event => {if (event.key === 'Enter' && !locked && path.trim()) onInspect();}}/></div>
          <button id="inspect" className="primary" disabled={locked || !path.trim()} title={lockReason ?? undefined} onClick={onInspect}>{g.inspect}</button></div>
        <div className="operation-status" role="status">{lockReason ?? ''}</div>
        {workspace.error && <><FaultMessage title={g.actionFailed} value={workspace.error}/>{workspace.recoveryOutcomes.length === 0 && <p className="muted">{g.inspectRecovery}</p>}</>}
      </div>
    </section>
  </>;
}

import {FaultMessage} from '../components/ResultPanel.tsx';
import {UNSUPPORTED_SOURCE} from '../workspace.ts';
import type {Workspace} from '../workspace.ts';
import {messages, renderMessage} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  workspace: Workspace; label: string;
  // True while another command or the closing application refuses new inspection; the path stays editable.
  locked: boolean; lockReason: string | null;
  onPath: (value: string) => void; onInspect: () => void;
  // Stop for a foreign owner lives in the strip; the page only says so.
  foreignOperation: boolean;
}

// Main content for a Tab without a usable package. Three honest states: no saved package (Edit guidance), a saved
// custom archive this desktop cannot inspect, or a saved directory whose inspection failed. Inspect is the one real
// action and is separate from creation; it never invents inventory.
export default function GuidancePage({workspace, label, locked, lockReason, onPath, onInspect, foreignOperation}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const g = t.guidance;
  const source = workspace.sourceError;
  const unsupported = source !== null && source.category === UNSUPPORTED_SOURCE;
  const eyebrow = source === null ? g.eyebrow : unsupported ? g.unsupportedEyebrow : g.unavailableEyebrow;
  const heading = source === null ? g.heading : unsupported ? g.unsupportedHeading : g.unavailableHeading;
  const path = workspace.inspectPath;
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.run.heading(label)}</span><h1>{heading}</h1>
      <p>{source === null ? g.intro : unsupported ? g.unsupportedHelp : g.unavailableHelp}</p></div></div>
    <div className="operation-status" role="status">{renderMessage(locale, workspace.busy ?? workspace.notice)}</div>
    <section className="panel guidance-panel" aria-labelledby="guidance-heading">
      <div className="panel-heading"><div><span className="eyebrow">{eyebrow}</span><h2 id="guidance-heading">{heading}</h2></div>
        <span className="tag">{workspace.internalName}</span></div>
      <div className="panel-body">
        {source === null && <p className="muted">{g.where}</p>}
        {source !== null && <FaultMessage title={heading} value={source}/>}
        {foreignOperation && <p className="authority-note">{g.stopHelp}</p>}
      </div>
    </section>
    <section className="panel open-form" aria-labelledby="inspect-heading">
      <div className="panel-body">
        <h2 id="inspect-heading">{g.inspectHeading}</h2>
        <p className="muted">{g.inspectHelp}</p>
        <div className="open-row"><div className="field"><label htmlFor="package-path">{g.packageDirectory}</label>
          <input id="package-path" type="text" value={path} disabled={workspace.busy !== null} placeholder={g.packagePlaceholder} spellCheck={false}
            onChange={event => onPath(event.target.value)} onKeyDown={event => {if (event.key === 'Enter' && !locked && path.trim()) onInspect();}}/></div>
          <button id="inspect" className="primary" disabled={locked || !path.trim()} title={lockReason ?? undefined} onClick={onInspect}>{g.inspect}</button></div>
        <div className="operation-status" role="status">{lockReason ?? ''}</div>
        {workspace.error && <><FaultMessage title={g.inspectFailed} value={workspace.error}/><p className="muted">{g.inspectRecovery}</p></>}
      </div>
    </section>
  </>;
}

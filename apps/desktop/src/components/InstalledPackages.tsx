import {useState} from 'react';
import {HelpTrigger} from './ContextualHelp.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {shortDigest} from '../distribution.ts';
import type {InstalledRelease} from '../distribution.ts';
import type {Fault, PackageSource} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  // The Tab's saved reference, to mark the release it names; display only.
  saved: PackageSource | null;
  // Unsaved profile, target or recovery edits a new selection would discard.
  dirty: boolean;
  releases: InstalledRelease[] | null; error: Fault | null; loading: boolean;
  // Why Import or Select cannot start now for this Tab.
  importBlock: string | null; selectBlock: string | null;
  onImport: () => void; onRefresh: () => void; onSelect: (release: InstalledRelease) => void;
}

// Installed signed releases of this data folder. Import never selects; Select is the explicit, Tab-scoped inspection
// that reverifies the installed release and passes the ordinary profile checks. Faulty entries stay attributable.
export default function InstalledPackages({saved, dirty, releases, error, loading, importBlock, selectBlock, onImport, onRefresh, onSelect}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const d = ui.distribution;
  const [confirm, setConfirm] = useState<InstalledRelease | null>(null);
  return <section id="installed-packages" className="panel open-form" aria-labelledby="installed-heading"><div className="panel-body">
    <h2 aria-labelledby="installed-heading"><span id="installed-heading">{d.installedHeading}</span>
      <HelpTrigger title={d.installedHeading} hint={d.installedHelp}><p>{d.installedHelp}</p></HelpTrigger></h2>
    <div className="button-row">
      <button id="installed-import" type="button" className="primary" aria-haspopup="dialog" disabled={importBlock !== null} title={importBlock ?? undefined}
        onClick={onImport}>{d.importPackage}</button>
      <button id="installed-refresh" type="button" disabled={loading} onClick={onRefresh}>{loading ? d.loading : d.refresh}</button>
    </div>
    {error && <FaultMessage title={d.listFailed} value={error}/>}
    {releases !== null && releases.length === 0 && <p id="installed-empty" className="muted">{d.installedEmpty}</p>}
    {releases !== null && releases.length > 0 && <ul id="installed-list" className="saved-list" aria-label={d.installedHeading}>
      {releases.map(release => {
        const facts = release.facts;
        const selected = saved?.kind === 'installed' && saved.package_id === release.package_id && saved.artifact_id === release.artifact_id;
        return <li key={`${release.package_id}/${release.artifact_id}`} className="saved-row installed-release"
          data-package-id={release.package_id} data-artifact-id={release.artifact_id}>
          <div className="saved-text">
            <strong>{facts ? facts.name : release.package_id}</strong>
            <span className="mono">{release.package_id}{facts ? ` · ${facts.release_version}` : ''}</span>
            <span className="mono" title={release.artifact_id}>{`${d.artifactLabel} ${shortDigest(release.artifact_id)}`}
              {facts ? ` · ${d.keyFingerprint} ${shortDigest(facts.key_id)}` : ''}</span>
            {release.fault && <FaultMessage title={d.entryFailed} value={release.fault}/>}
          </div>
          {selected && <span className="tag current">{d.selected}</span>}
          {facts && <button type="button" className="installed-select" disabled={selectBlock !== null || confirm !== null} title={selectBlock ?? undefined}
            aria-label={d.selectLabel(facts.name, facts.release_version)} onClick={() => dirty ? setConfirm(release) : onSelect(release)}>{d.select}</button>}
        </li>;
      })}
    </ul>}
    {confirm && <div className="confirm-row" role="alertdialog" aria-labelledby="installed-confirm-text">
      <span id="installed-confirm-text">{d.confirmSelect}</span>
      <button id="installed-discard" type="button" className="danger-text" disabled={selectBlock !== null}
        onClick={() => {setConfirm(null); onSelect(confirm);}}>{d.discardSelect}</button>
      <button id="installed-keep" type="button" autoFocus onClick={() => setConfirm(null)}>{ui.run.keepDraft}</button></div>}
    <div id="installed-block" className="muted" role="status">{selectBlock ?? ''}</div>
  </div></section>;
}

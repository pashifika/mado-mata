import {useEffect, useMemo, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {AuthoringFooterStatus} from './StatusSurface.tsx';
import type {AuthoringStatusProps} from './StatusSurface.tsx';
import {shortDigest} from '../distribution.ts';
import type {InstalledRelease} from '../distribution.ts';
import {buildLibrary, filterLibrary, initialArtifact, libraryOutcome, locate, resolveHighlight, sameArtifact, savedArtifact} from '../package-library.ts';
import type {ArtifactRef, Highlight, LibraryFilter, LibraryGroup} from '../package-library.ts';
import type {Fault, PackageSource} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import './package-library.css';

interface Props {
  // Open for the Workspace the parent captured; each new opening starts a fresh browse.
  open: boolean;
  // Import or recovery owns the modal: the dialog hides and keeps its browsing state for the same opening.
  suspended: boolean;
  // Label of the captured Workspace that a selection targets.
  owner: string;
  // That Workspace's saved package source and whether a selection would discard its unsaved edits.
  saved: PackageSource | null; dirty: boolean;
  // The shared data-root listing: null until a read succeeds; `error` is the last read failure.
  releases: InstalledRelease[] | null; error: Fault | null; loading: boolean;
  // Why Import or selection cannot start now for the captured Workspace, including a retired owner.
  importBlock: string | null; selectBlock: string | null;
  onImport: () => void; onRefresh: () => void; onCancel: () => void;
  // Explicit selection of one exact verified artifact; the parent runs the installed inspection for its owner.
  onSelect: (release: InstalledRelease) => void;
  // Opens diagnosis of one installed artifact; null hides the entry.
  onDiagnose: ((target: ArtifactRef) => void) | null;
  // Diagnosis shown in place of list and detail; Back returns to the unchanged browse.
  diagnosis: ReactNode | null; onBackFromDiagnosis: () => void;
  // Focus target after closing when the opener is gone.
  returnFocus: () => HTMLElement | null;
  strip: ReactNode; authoring: AuthoringStatusProps | null;
}

type FocusRequest = {kind: 'selector'; selector: string} | {kind: 'row'; key: string} | {kind: 'detail'};

// Neutral fallbacks for lost focus, in order; never Stop, selection or another mutation.
const NEUTRAL = ['#package-library-search', '#package-library-diagnosis-back', '#close-package-library'];

const usable = (element: Element | null): element is HTMLElement => element instanceof HTMLElement && element.isConnected
  && element.getClientRects().length > 0 && !element.matches(':disabled');

function rowKey(highlight: Highlight): string {
  return highlight.kind === 'group' ? highlight.id : JSON.stringify(['problem', highlight.package_id, highlight.artifact_id]);
}

// Installed signed releases of this data folder for one captured Workspace. Searching, highlighting, opening details and
// diagnosis never select; one exact verified artifact is selected only by the footer action, which the parent
// dispatches through the ordinary installed inspection. Entries without verified facts stay separate problems.
export default function PackageLibraryDialog({open, suspended, owner, saved, dirty, releases, error, loading, importBlock, selectBlock,
  onImport, onRefresh, onCancel, onSelect, onDiagnose, diagnosis, onBackFromDiagnosis, returnFocus, strip, authoring}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const d = ui.distribution;
  const p = ui.packageLibrary;
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<LibraryFilter>('all');
  const [highlight, setHighlight] = useState<Highlight | null>(null);
  const [pane, setPane] = useState<'list' | 'detail'>('list');
  const [confirm, setConfirm] = useState<ArtifactRef | null>(null);
  const [focusRequest, setFocusRequest] = useState<FocusRequest | null>(null);
  const list = useRef<HTMLDivElement>(null);
  // The list's scroll position survives hiding (narrow detail, suspension, diagnosis) and is restored once shown.
  const listScroll = useRef(0);
  const restoreScroll = useRef(false);
  const diagnosedFrom = useRef<FocusRequest | null>(null);

  const visible = open && !suspended;
  const diagnosing = diagnosis !== null;
  const [seen, setSeen] = useState({open, visible, diagnosing});
  if (seen.open !== open || seen.visible !== visible || seen.diagnosing !== diagnosing) {
    setSeen({open, visible, diagnosing});
    if (open && !seen.open) {
      setQuery('');
      setFilter('all');
      setHighlight(null);
      setPane('list');
      setConfirm(null);
      setFocusRequest(null);
      listScroll.current = 0;
      restoreScroll.current = false;
    } else {
      if (visible && !seen.visible) {
        restoreScroll.current = true;
        setFocusRequest({kind: 'selector', selector: '#installed-import'});
      }
      if (diagnosing && !seen.diagnosing) setFocusRequest({kind: 'selector', selector: '#package-library-diagnosis-back'});
      else if (!diagnosing && seen.diagnosing) {
        restoreScroll.current = true;
        setFocusRequest(diagnosedFrom.current);
      }
    }
  }

  // Derived from the in-memory listing only; typing and highlighting never reread the host.
  const library = useMemo(() => buildLibrary(releases ?? []), [releases]);
  const shown = useMemo(() => filterLibrary(library, query, filter), [library, query, filter]);
  const outcome = libraryOutcome(releases, error, loading, shown, query);
  const resolved = resolveHighlight(shown, highlight);
  const target = resolved?.kind === 'group' ? resolved.version : null;
  const savedRef = savedArtifact(saved);
  const savedPlace = savedRef === null ? null : locate(library, savedRef);
  const savedUnlisted = saved?.kind === 'installed' && releases !== null && savedPlace === null;
  const confirming = dirty && confirm !== null && target !== null && sameArtifact(confirm, target.release);

  useEffect(() => {
    const dialog = document.getElementById('package-library');
    if (!visible || !(dialog instanceof HTMLDialogElement) || !dialog.open) return;
    const scroller = list.current;
    const listShown = scroller !== null && scroller.getClientRects().length > 0;
    if (restoreScroll.current && listShown) {
      scroller.scrollTop = listScroll.current;
      restoreScroll.current = false;
    }
    if (focusRequest !== null) {
      setFocusRequest(null);
      const requested = focusRequest.kind === 'selector' ? dialog.querySelector(focusRequest.selector)
        : focusRequest.kind === 'row' ? Array.from(dialog.querySelectorAll<HTMLElement>('[data-row]')).find(row => row.dataset.row === focusRequest.key) ?? null
        // Detail takes focus only when it replaced the list; side by side, focus stays on the activated row.
        : listShown ? null : dialog.querySelector('#package-library-detail-heading');
      if (usable(requested)) {
        requested.focus();
        return;
      }
    }
    const active = document.activeElement;
    if (active !== dialog && dialog.contains(active) && usable(active)) return;
    for (const selector of NEUTRAL) {
      const element = dialog.querySelector(selector);
      if (usable(element)) {
        element.focus();
        return;
      }
    }
  });

  function openGroup(group: LibraryGroup) {
    setHighlight(current => current?.kind === 'group' && current.id === group.id ? current
      : {kind: 'group', id: group.id, artifact: initialArtifact(group, savedRef)});
    setPane('detail');
    setFocusRequest({kind: 'detail'});
  }
  function openProblem(release: InstalledRelease) {
    setHighlight({kind: 'problem', package_id: release.package_id, artifact_id: release.artifact_id});
    setPane('detail');
    setFocusRequest({kind: 'detail'});
  }
  function back() {
    setPane('list');
    restoreScroll.current = true;
    setFocusRequest(highlight === null ? {kind: 'selector', selector: '#package-library-search'} : {kind: 'row', key: rowKey(highlight)});
  }
  function diagnose(artifact: ArtifactRef, control: string) {
    if (onDiagnose === null) return;
    diagnosedFrom.current = {kind: 'selector', selector: control};
    onDiagnose({package_id: artifact.package_id, artifact_id: artifact.artifact_id});
  }
  function select() {
    if (target === null || selectBlock !== null) return;
    if (dirty) {
      setConfirm({package_id: target.release.package_id, artifact_id: target.release.artifact_id});
      setFocusRequest({kind: 'selector', selector: '#package-library-keep'});
    } else onSelect(target.release);
  }
  function discardAndSelect() {
    if (target === null || selectBlock !== null) return;
    setConfirm(null);
    onSelect(target.release);
  }
  function keepDraft() {
    setConfirm(null);
    setFocusRequest({kind: 'selector', selector: '#package-library-select'});
  }

  const savedTag = <span className="tag current">{d.selected}</span>;
  const problemTag = <span className="tag status-attention">{p.problem}</span>;
  const savedLabel = savedRef === null ? '' : `${savedRef.package_id} · ${shortDigest(savedRef.artifact_id)}`;
  const current = saved === null ? p.currentNone
    : saved.kind === 'directory' ? p.currentDirectory(saved.path)
    : saved.kind === 'custom_archive' ? p.currentArchive(saved.path)
    : savedPlace?.kind === 'group' && savedPlace.version ? p.currentInstalled(savedPlace.version.facts.name, savedPlace.version.facts.release_version)
    : savedUnlisted ? p.currentInstalledUnlisted(savedLabel) : d.installedSource(savedLabel);

  const listPane = <div ref={list} className="package-library-list" onScroll={event => {
    if (!restoreScroll.current && event.currentTarget.getClientRects().length > 0) listScroll.current = event.currentTarget.scrollTop;
  }}>
    {savedUnlisted && <div id="package-library-saved-unlisted" className="inline-warning">
      <p>{p.currentUnlistedHelp}</p>
      {onDiagnose && savedRef && <button id="package-library-diagnose-saved" type="button"
        onClick={() => diagnose(savedRef, '#package-library-diagnose-saved')}>{p.diagnose}</button>}
    </div>}
    {error && <FaultMessage title={d.listFailed} value={error}/>}
    {outcome === 'unread' && <p className="muted">{p.unread}</p>}
    {outcome === 'loading' && <p className="muted">{d.loading}</p>}
    {outcome === 'failed' && <p className="muted">{p.listUnavailable}</p>}
    {outcome === 'empty' && <p id="package-library-empty" className="muted">{p.empty}</p>}
    {outcome === 'noMatches' && <p id="package-library-no-matches" className="muted">{p.noMatches(query.trim())}</p>}
    {outcome === 'noProblems' && <p className="muted">{p.noProblems}</p>}
    {shown.groups.length > 0 && <ul id="package-library-groups" className="package-library-rows" aria-label={d.installedHeading}>
      {shown.groups.map(group => {
        const highlighted = resolved?.kind === 'group' && resolved.group.id === group.id;
        return <li key={group.id}>
          <button type="button" className="package-library-row" data-row={group.id} aria-current={highlighted ? 'true' : undefined} onClick={() => openGroup(group)}>
            <span className="package-library-row-title"><strong>{group.names.join(' · ')}</strong>
              {group.versions.some(version => sameArtifact(version.release, savedRef)) && savedTag}</span>
            <span className="mono">{group.package_id}</span>
            <span className="muted">{`${p.versions(group.versions.length)} · ${p.signer(shortDigest(group.key_id))}`}</span>
          </button>
        </li>;
      })}
    </ul>}
    {shown.problems.length > 0 && <section aria-labelledby="package-library-problems-heading">
      <h3 id="package-library-problems-heading">{p.problemsHeading}</h3>
      <ul id="package-library-problems" className="package-library-rows">
        {shown.problems.map(release => {
          const key = rowKey({kind: 'problem', package_id: release.package_id, artifact_id: release.artifact_id});
          const highlighted = resolved?.kind === 'problem' && resolved.release === release;
          return <li key={key}>
            <button type="button" className="package-library-row problem" data-row={key} aria-current={highlighted ? 'true' : undefined} onClick={() => openProblem(release)}>
              <span className="package-library-row-title"><strong className="mono">{release.package_id}</strong>{problemTag}
                {sameArtifact(release, savedRef) && savedTag}</span>
              <span className="mono">{`${d.artifactLabel} ${shortDigest(release.artifact_id)}`}</span>
              {release.fault && <span className="muted">{release.fault.category}</span>}
            </button>
          </li>;
        })}
      </ul>
    </section>}
    <span className="visually-hidden" role="status">{outcome === 'shown' && query.trim() !== '' ? p.results(shown.groups.length + shown.problems.length) : ''}</span>
  </div>;

  let detail: ReactNode;
  if (resolved === null) {
    detail = <><p className="muted">{p.detailEmpty}</p><p className="muted">{d.installedHelp}</p></>;
  } else if (resolved.kind === 'problem') {
    const release = resolved.release;
    detail = <>
      <h3 id="package-library-detail-heading" className="mono" tabIndex={-1}>{release.package_id}</h3>
      <p className="package-library-tags">{problemTag}{sameArtifact(release, savedRef) && savedTag}</p>
      <dl className="package-library-facts">
        <dt>{d.packageId}</dt><dd className="mono">{release.package_id}</dd>
        <dt>{d.artifact}</dt><dd className="mono">{release.artifact_id}</dd>
      </dl>
      <p className="muted">{p.problemHelp}</p>
      {release.fault ? <FaultMessage title={d.entryFailed} value={release.fault}/> : <p className="inline-warning">{p.noFacts}</p>}
      {onDiagnose && <button id="package-library-diagnose" type="button" onClick={() => diagnose(release, '#package-library-diagnose')}>{p.diagnose}</button>}
    </>;
  } else {
    const {group, version} = resolved;
    detail = <>
      <h3 id="package-library-detail-heading" tabIndex={-1}>{group.names.join(' · ')}</h3>
      <dl className="package-library-facts">
        <dt>{d.packageId}</dt><dd className="mono">{group.package_id}</dd>
        <dt>{d.keyFingerprint}</dt><dd className="mono">{group.key_id}</dd>
        <dt>{d.signature}</dt><dd>{d.signatureValid}</dd>
        <dt>{d.account}</dt><dd>{d.accountUnverified}</dd>
      </dl>
      <fieldset className="package-library-versions">
        <legend>{p.versionsLegend}</legend>
        {group.versions.map(item => {
          const artifact = item.release.artifact_id;
          return <label key={artifact} className="package-library-version">
            <input type="radio" name="package-library-version" value={artifact} checked={version?.release.artifact_id === artifact}
              onChange={() => setHighlight({kind: 'group', id: group.id, artifact})}/>
            <span className="package-library-version-text"><strong>{item.facts.release_version}</strong>
              <span className="mono">{shortDigest(artifact)}</span>
              {group.names.length > 1 && <span>{item.facts.name}</span>}
              {sameArtifact(item.release, savedRef) && savedTag}</span>
          </label>;
        })}
      </fieldset>
      {version && <section className="package-library-version-detail" aria-labelledby="package-library-version-heading">
        <h4 id="package-library-version-heading">{`${d.release} ${version.facts.release_version}`}</h4>
        <dl className="package-library-facts">
          <dt>{d.name}</dt><dd>{version.facts.name}</dd>
          {version.facts.description !== '' && <><dt>{d.description}</dt><dd className="package-library-description">{version.facts.description}</dd></>}
          {Object.keys(version.facts.game_names).length > 0 && <><dt>{d.gameNames}</dt><dd><ul className="package-library-game-names">
            {Object.entries(version.facts.game_names).map(([key, name]) => <li key={key}><span className="mono">{key}</span> {name}</li>)}
          </ul></dd></>}
          <dt>{d.revision}</dt><dd className="mono">{version.facts.revision}</dd>
          <dt>{d.artifact}</dt><dd className="mono">{version.release.artifact_id}</dd>
          <dt>{d.content}</dt><dd>{d.contentSummary(version.facts.files, version.facts.bytes)}</dd>
        </dl>
        <p className="muted">{p.verifiedWhenListed}</p>
      </section>}
    </>;
  }

  return <Modal id="package-library" className="package-library" open={visible} labelledBy="package-library-heading"
    onCancel={() => {if (confirming) keepDraft(); else onCancel();}} initialFocus="#package-library-search" returnFocus={returnFocus}>
    <div className="dialog-header">
      <div className="package-library-heading"><span className="eyebrow">{owner}</span><h2 id="package-library-heading">{d.installedHeading}</h2>
        <p id="package-library-current">{`${p.current}: ${current}`}{savedPlace?.kind === 'problem' && problemTag}</p></div>
      <button type="button" className="icon" aria-label={p.close} title={p.close} onClick={onCancel}>×</button>
    </div>
    {strip}
    <div className="package-library-main">
      {diagnosing
        ? <>
          <div className="package-library-nav"><button id="package-library-diagnosis-back" type="button" onClick={onBackFromDiagnosis}>
            <span aria-hidden="true">← </span>{p.diagnosisBack}</button></div>
          <div className="package-library-scroll">{diagnosis}</div>
        </>
        : <>
          <div className="package-library-toolbar">
            <label htmlFor="package-library-search" className="visually-hidden">{p.searchLabel}</label>
            <input id="package-library-search" type="search" value={query} placeholder={p.searchPlaceholder} spellCheck={false}
              autoCapitalize="off" autoCorrect="off" autoComplete="off" onChange={event => setQuery(event.target.value)}/>
            <div className="package-library-filter" role="group" aria-label={p.filterLabel}>
              <button type="button" aria-pressed={filter === 'all'} onClick={() => setFilter('all')}>{p.filterAll}</button>
              <button type="button" aria-pressed={filter === 'problems'} onClick={() => setFilter('problems')}>{p.filterProblems(library.problems.length)}</button>
            </div>
            <button id="package-library-refresh" type="button" disabled={loading} onClick={onRefresh}>{loading ? d.loading : d.refresh}</button>
            <button id="installed-import" type="button" aria-haspopup="dialog" disabled={importBlock !== null} title={importBlock ?? undefined}
              onClick={onImport}>{d.importPackage}</button>
          </div>
          <div className="package-library-body" data-pane={pane}>
            {listPane}
            <div className="package-library-detail">
              <div className="package-library-back-row"><button id="package-library-back" type="button" onClick={back}>
                <span aria-hidden="true">← </span>{p.back}</button></div>
              <div className="package-library-scroll">{detail}</div>
            </div>
          </div>
        </>}
    </div>
    <div className="dialog-footer package-library-footer">
      {!diagnosing && (confirming
        ? <div className="confirm-row package-library-target" role="alertdialog" aria-labelledby="package-library-confirm-text">
          <span id="package-library-confirm-text">{d.confirmSelect}</span>
          <button id="package-library-discard" type="button" className="danger-text" disabled={selectBlock !== null} onClick={discardAndSelect}>{d.discardSelect}</button>
          <button id="package-library-keep" type="button" onClick={keepDraft}>{ui.run.keepDraft}</button>
        </div>
        : <p id="package-library-target" className="package-library-target">{target
          ? p.target(target.facts.name, target.facts.release_version, shortDigest(target.release.artifact_id), owner) : p.chooseVersion(owner)}</p>)}
      <span id="package-library-status" role="status">{selectBlock ?? ''}</span>
      {authoring && <AuthoringFooterStatus key={authoring.activity.owner.token} {...authoring} idPrefix="package-library-authoring"/>}
      <button id="close-package-library" type="button" onClick={onCancel}>{ui.common.close}</button>
      {!diagnosing && !confirming && <button id="package-library-select" type="button" className="primary" disabled={target === null || selectBlock !== null}
        title={selectBlock ?? undefined} aria-label={target ? p.selectForLabel(target.facts.name, target.facts.release_version, owner) : undefined}
        onClick={select}>{p.selectFor(owner)}</button>}
    </div>
  </Modal>;
}

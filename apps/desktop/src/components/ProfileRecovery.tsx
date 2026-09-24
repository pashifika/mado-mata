import {useMemo, useRef, useState} from 'react';
import SchemaForm from './SchemaForm.tsx';
import Select from './Select.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {issuePath, recoveryIssue} from '../recovery.ts';
import type {RecoveryState} from '../recovery.ts';
import {readDraft} from '../state.ts';
import type {Json, ProfileRecoveryOutcome} from '../types.ts';

export interface RecoveryHandlers {
  // Selecting (re)loads the stored safe values; null releases a draft whose profile is no longer rejected.
  select:(id:string|null) => void; edit:(values:Record<string,Json>) => void;
  save:() => void; reset:() => void; retry:() => void; discard:() => void;
}

interface Props {
  // Field IDs are prefixed so this editor and the ordinary profile editor can share one page.
  idPrefix:string;
  // Tab label naming the owner in the Reset confirmation.
  label:string;
  state:RecoveryState|null; outcomes:ProfileRecoveryOutcome[];
  locked:boolean; handlers:RecoveryHandlers;
}

// Shared by the Run page and the missing-source guidance; a recovery context is never a Selection or run authority.
export default function ProfileRecovery({idPrefix, label, state, outcomes, locked, handlers}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui.recovery;
  // Confirmation belongs to one draft; a selection change or edit under it withdraws the question.
  const [confirm, setConfirm] = useState<{profileId:string; draftRevision:number}|null>(null);
  const resetButton = useRef<HTMLButtonElement>(null);
  const view = state?.view ?? null;
  const selected = state && state.selectedId !== null ? state.view.profiles.find(entry => entry.profile.id === state.selectedId) : undefined;
  const parsed = useMemo(() => state ? readDraft(state.view.package.schema, state.draft, locale) : null, [state?.view.package.schema, state?.draft, locale]);
  if (!state || !view || !parsed) {
    return outcomes.length > 0 ? <Outcomes idPrefix={idPrefix} outcomes={outcomes} heading={t.heading}/> : null;
  }
  const numericErrors = Object.keys(parsed.errors).length > 0;
  const issue = recoveryIssue(state);
  const path = issue ? issuePath(issue.fault) : null;
  // A draft kept for a profile the refreshed listing no longer rejects: edits made after its save, still unsaved.
  const detached = state.selectedId !== null && !selected;
  const confirming = confirm !== null && selected !== undefined && confirm.profileId === selected.profile.id && confirm.draftRevision === state.draftRevision;
  function closeConfirm(reset:boolean) {
    setConfirm(null);
    if (reset) handlers.reset(); else resetButton.current?.focus();
  }
  return <section id={`${idPrefix}-panel`} className="panel recovery-panel" aria-labelledby={`${idPrefix}-heading`}>
    <div className="panel-heading"><div><span className="eyebrow">{t.eyebrow}</span><h2 id={`${idPrefix}-heading`}>{view.relocation ? t.relocationHeading : t.heading}</h2></div>
      <span className={`tag ${view.relocation ? 'stale' : ''}`}>{view.package.package_id}</span></div>
    <div className="panel-body">
      <p className="field-help">{view.relocation ? t.relocationHelp : t.help}</p>
      <dl className="run-identity"><dt>{view.relocation ? t.candidate : t.source}</dt><dd className="mono">{view.package_path}</dd>
        <dt>{t.inspectedSchema}</dt><dd><code>{view.package.schema_identity}</code></dd></dl>
      {outcomes.length > 0 && <Outcomes idPrefix={idPrefix} outcomes={outcomes} heading={null}/>}
      {view.profiles_error && <FaultMessage title={t.listingFailed} value={view.profiles_error}/>}
      {state.refreshError && <p className="inline-warning">{t.refreshFailed}</p>}
      {view.profiles.length === 0 && !detached && <p id={`${idPrefix}-empty`} className="muted">{view.relocation ? t.allRepaired : t.noneRejected}</p>}
      {view.profiles.length > 0 && <div className="field"><label htmlFor={`${idPrefix}-profile`}>{t.rejectedProfile}</label>
        <Select id={`${idPrefix}-profile`} value={selected ? selected.profile.id : ''} disabled={locked} onChange={id => handlers.select(id || null)}
          options={[{value:'', label:t.chooseProfile}, ...view.profiles.map(entry => ({value:entry.profile.id, label:entry.profile.name, description:entry.issue.category}))]}/></div>}
      {detached && <div className="inline-warning">{t.noLongerRejected}
        <button id={`${idPrefix}-discard-draft`} type="button" disabled={locked} onClick={() => handlers.select(null)}>{t.discardDraft}</button></div>}
      {(selected || detached) && <>
        {selected && <dl className="run-identity"><dt>{t.profileId}</dt><dd id={`${idPrefix}-profile-id`}>{selected.profile.id}</dd>
          <dt>{t.storedSchema}</dt><dd><code>{selected.profile.schema_identity}</code></dd></dl>}
        {issue && <>
          <FaultMessage title={t.reason} value={issue.fault}/>
          {issue.earlier && <p className="inline-warning">{t.earlier}</p>}
          {path && <p id={`${idPrefix}-path`} className="identity-text">{t.valuePath}: <code>{path}</code></p>}
        </>}
        {state.resetDraft && <p className="inline-warning">{t.resetIncomplete}</p>}
        <fieldset className="recovery-fields" disabled={locked}>
          <legend>{t.values}</legend>
          <p className="field-help">{t.valuesHelp}</p>
          <SchemaForm idPrefix={`${idPrefix}-option`} schema={view.package.schema} value={state.draft} onChange={handlers.edit} errors={parsed.errors}/>
        </fieldset>
        <div className="button-row">
          <button id={`${idPrefix}-save`} type="button" className="primary" disabled={locked || numericErrors || !selected} onClick={handlers.save}>{t.save}</button>
          <button id={`${idPrefix}-reload`} type="button" disabled={locked || !selected || (!state.touched && !state.resetDraft)} onClick={() => {if (selected) handlers.select(selected.profile.id);}}>{t.reload}</button>
          {selected && (confirming
            ? <div className="confirm-row" role="alertdialog" aria-labelledby={`${idPrefix}-confirm-text`}>
              <span id={`${idPrefix}-confirm-text`}>{t.confirmReset(selected.profile.name, selected.profile.id, label, view.package.package_id)}</span>
              <button id={`${idPrefix}-reset-confirm`} type="button" className="danger-text" disabled={locked} onClick={() => closeConfirm(true)}>{t.resetConfirmed}</button>
              <button id={`${idPrefix}-reset-cancel`} type="button" autoFocus onClick={() => closeConfirm(false)}>{t.cancel}</button></div>
            : <button id={`${idPrefix}-reset`} ref={resetButton} type="button" className="danger-text" disabled={locked}
              onClick={() => setConfirm({profileId:selected.profile.id, draftRevision:state.draftRevision})}>{t.reset}</button>)}
        </div>
      </>}
      <div className="button-row">
        {view.relocation && <button id={`${idPrefix}-retry`} type="button" className="primary" disabled={locked} onClick={handlers.retry}>{t.retry}</button>}
        <button id={`${idPrefix}-discard`} type="button" disabled={locked} onClick={handlers.discard}>{view.relocation ? t.discardCandidate : t.close}</button>
        {view.relocation && <span className="muted">{t.retryHelp}</span>}
      </div>
      <p className="muted">{t.discardHelp}</p>
    </div>
  </section>;
}

// Committed saves stay visible after the context is discarded or superseded.
function Outcomes({idPrefix, outcomes, heading}: {idPrefix:string; outcomes:ProfileRecoveryOutcome[]; heading:string|null}) {
  const locale = useLocale();
  const t = messages[locale].ui.recovery;
  const list = <>
    <h3>{t.outcomes}</h3>
    <ul id={`${idPrefix}-outcomes`} className="recovery-outcomes">{outcomes.map(outcome => {
      const path = outcome.issue ? issuePath(outcome.issue) : null;
      return <li key={outcome.profile_id} data-status={outcome.status}>
        <span className={`tag ${outcome.status === 'saved' ? 'current' : outcome.status === 'storage_failed' ? 'stale' : 'unsaved'}`}>{t.outcomeStatus(outcome.status)}</span>
        <strong>{outcome.name}</strong><code>{outcome.profile_id}</code>
        {outcome.issue && <span className="muted">{outcome.issue.category} · {outcome.issue.message}{path ? ` · ${path}` : ''}</span>}
      </li>;
    })}</ul>
    <p className="muted">{t.outcomesHelp}</p>
  </>;
  if (heading === null) return list;
  return <section id={`${idPrefix}-panel`} className="panel recovery-panel" aria-labelledby={`${idPrefix}-heading`}>
    <div className="panel-heading"><div><span className="eyebrow">{t.eyebrow}</span><h2 id={`${idPrefix}-heading`}>{heading}</h2></div></div>
    <div className="panel-body">{list}</div>
  </section>;
}

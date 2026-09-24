import {useEffect, useMemo, useRef, useState} from 'react';
import SchemaForm from './SchemaForm.tsx';
import Select from './Select.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {issuePath, readRecoveryDraft, recoveryIssue, sameRecoveryContext} from '../recovery.ts';
import type {RecoveryEdit, RecoveryState} from '../recovery.ts';
import type {Json, ProfileRecoveryOutcome, RecoveryRef} from '../types.ts';

export interface RecoveryHandlers {
  // Selecting (re)loads the stored safe values; null releases a draft whose profile is no longer rejected.
  select:(id:string|null) => void; edit:(values:Record<string,Json>, change:RecoveryEdit) => void;
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
  // Confirmation belongs to one draft of one context: another Tab's context, a selection change or an edit under it
  // withdraws the question instead of asking it about a profile the operator never chose.
  const [confirm, setConfirm] = useState<{context:RecoveryRef; profileId:string; draftRevision:number}|null>(null);
  const resetButton = useRef<HTMLButtonElement>(null);
  const confirmRow = useRef<HTMLDivElement>(null);
  // Set only when the confirmation closes while it owns focus. Focus returns to the remounted Reset button; once a
  // confirmed Reset locks it, the stable page tab takes focus instead of the document body.
  const refocus = useRef(false);
  const view = state?.view ?? null;
  const selected = state && state.selectedId !== null ? state.view.profiles.find(entry => entry.profile.id === state.selectedId) : undefined;
  const parsed = useMemo(() => state ? readRecoveryDraft(state, locale) : null, [state, locale]);
  const stored = useMemo(() => state ? new Set(state.storedText.map(entry => entry.path)) : null, [state?.storedText]);
  const confirming = confirm !== null && state !== null && selected !== undefined && sameRecoveryContext(confirm.context, state.view.context)
    && confirm.profileId === selected.profile.id && confirm.draftRevision === state.draftRevision;
  useEffect(() => {
    if (confirming || !refocus.current) return;
    refocus.current = false;
    const button = resetButton.current;
    (button && !button.disabled ? button : document.getElementById('page-run'))?.focus();
  }, [confirming]);
  if (!state || !view || !parsed || !stored) {
    return outcomes.length > 0 ? <Outcomes idPrefix={idPrefix} outcomes={outcomes} heading={t.outcomes}/> : null;
  }
  const fieldErrors = Object.keys(parsed.errors).length > 0;
  const issue = recoveryIssue(state);
  const path = issue ? issuePath(issue.fault) : null;
  // A draft kept for a profile the refreshed listing no longer rejects: edits made after its save, still unsaved.
  const detached = state.selectedId !== null && !selected;
  // Binding is owed after a recovery-required or failed binding; a relocation is the strict same-package candidate case.
  const pending = view.binding_required;
  const heading = view.relocation ? t.relocationHeading : pending ? t.bindingHeading : t.heading;
  const help = view.relocation ? t.relocationHelp : pending ? t.bindingHelp : t.help;
  function closeConfirm(reset:boolean) {
    refocus.current = confirmRow.current?.contains(document.activeElement) ?? false;
    setConfirm(null);
    if (reset) handlers.reset();
  }
  return <section id={`${idPrefix}-panel`} className="panel recovery-panel" aria-labelledby={`${idPrefix}-heading`}>
    <div className="panel-heading"><div><span className="eyebrow">{t.eyebrow}</span><h2 id={`${idPrefix}-heading`}>{heading}</h2></div>
      <span className={`tag ${pending ? 'stale' : ''}`}>{view.package.package_id}</span></div>
    <div className="panel-body">
      <p className="field-help">{help}</p>
      <dl className="run-identity"><dt>{view.relocation ? t.candidate : t.source}</dt><dd className="mono">{view.package_path}</dd>
        <dt>{t.inspectedSchema}</dt><dd><code>{view.package.schema_identity}</code></dd></dl>
      {outcomes.length > 0 && <Outcomes idPrefix={idPrefix} outcomes={outcomes} heading={null}/>}
      {view.profiles_error && <FaultMessage title={t.listingFailed} value={view.profiles_error}/>}
      {state.refreshError && <p className="inline-warning">{t.refreshFailed}</p>}
      {view.profiles.length === 0 && !detached && <p id={`${idPrefix}-empty`} className="muted">{pending ? t.allRepaired : t.noneRejected}</p>}
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
          <SchemaForm idPrefix={`${idPrefix}-option`} schema={view.package.schema} value={state.draft} onChange={handlers.edit} errors={parsed.errors} stored={stored}/>
        </fieldset>
        <div className="button-row">
          <button id={`${idPrefix}-save`} type="button" className="primary" disabled={locked || fieldErrors || !selected} onClick={handlers.save}>{t.save}</button>
          <button id={`${idPrefix}-reload`} type="button" disabled={locked || !selected || (!state.touched && !state.resetDraft)} onClick={() => {if (selected) handlers.select(selected.profile.id);}}>{t.reload}</button>
          {selected && (confirming
            ? <div ref={confirmRow} className="confirm-row" role="alertdialog" aria-labelledby={`${idPrefix}-confirm-text`}>
              <span id={`${idPrefix}-confirm-text`}>{t.confirmReset(selected.profile.name, selected.profile.id, label, view.package.package_id)}</span>
              <button id={`${idPrefix}-reset-confirm`} type="button" className="danger-text" disabled={locked} onClick={() => closeConfirm(true)}>{t.resetConfirmed}</button>
              <button id={`${idPrefix}-reset-cancel`} type="button" autoFocus onClick={() => closeConfirm(false)}>{t.cancel}</button></div>
            : <button id={`${idPrefix}-reset`} ref={resetButton} type="button" className="danger-text" disabled={locked}
              onClick={() => setConfirm({context:view.context, profileId:selected.profile.id, draftRevision:state.draftRevision})}>{t.reset}</button>)}
        </div>
      </>}
      <div className="button-row">
        {pending && <button id={`${idPrefix}-retry`} type="button" className="primary" disabled={locked} onClick={handlers.retry}>{t.retry}</button>}
        <button id={`${idPrefix}-discard`} type="button" disabled={locked} onClick={handlers.discard}>{pending ? t.discardCandidate : t.close}</button>
        {pending && <span className="muted">{t.retryHelp}</span>}
      </div>
      <p className="muted">{t.discardHelp}</p>
    </div>
  </section>;
}

// Committed saves stay visible after the context is discarded or superseded. Without a context the list is the whole
// panel under a neutral outcomes heading: automatic saves are not "rejected profiles".
function Outcomes({idPrefix, outcomes, heading}: {idPrefix:string; outcomes:ProfileRecoveryOutcome[]; heading:string|null}) {
  const locale = useLocale();
  const t = messages[locale].ui.recovery;
  const list = <>
    {heading === null && <h3>{t.outcomes}</h3>}
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

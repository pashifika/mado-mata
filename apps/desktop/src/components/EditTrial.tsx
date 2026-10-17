import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {functionKey, trialArguments, trialDiagnosticLocation, trialDiagnostics} from '../edit-trial.ts';
import type {EditTrialState} from '../edit-trial.ts';
import type {PackageValidation} from '../status.ts';
import type {Bound, Derived, NativeConsent} from '../workspace.ts';
import type {Fault} from '../types.ts';
import {InfoHint} from './ContextualHelp.tsx';
import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import Select from './Select.tsx';
import {AuthoringFooterStatus, ValidationStatusIcon} from './StatusSurface.tsx';
import type {AuthoringStatusProps} from './StatusSurface.tsx';
import type {TargetHandlers} from './TargetPanel.tsx';

export interface EditTrialHandlers {
  change:(update:(state:EditTrialState)=>EditTrialState)=>void;
  bound:(update:(bound:Bound)=>Bound)=>void;
  reload:()=>void; run:()=>void;
  native:{edit:(field:'operation'|'postcondition'|'workflowSeconds', value:string)=>void; approve:(field:NativeConsent, value:boolean)=>void};
  target:TargetHandlers;
  logs:()=>void;
  // This owner's full execution setup in Run, and its results in Execution.
  setup:()=>void; results:()=>void;
}

// Edit's Script execution entry: a Setup trigger beside Validate and a compact dialog over the same trial state.
// Profile, lane, options, target and Native review belong to the execution setup in Run.
export default function EditTrial({state, facts, handlers, locked, loadBlocked, loading, block, dirty, validation, error, errorSource, onDiagnostic, notice, hasResult, strip, authoring}: {
  state:EditTrialState|null; facts:Derived|null; handlers:EditTrialHandlers; locked:boolean; loadBlocked:boolean; loading:boolean; block:string|null; dirty:boolean;
  // Saved-package validation only; it never states that execution is ready.
  validation:PackageValidation['state'];
  error:Fault|null; errorSource?:{revision:string; stale:boolean; paths:readonly string[]}|null; onDiagnostic?:(diagnostic:Fault)=>void;
  notice:string|null; hasResult:boolean;
  // Shared activity controls belong inside the dialog: the modal makes the main window's footer inert.
  // `leave` closes the dialog before the given navigation runs.
  strip:ReactNode; authoring:(leave:(then:()=>void)=>void)=>AuthoringStatusProps|null;
}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const s = t.scriptTrial;
  const [open, setOpen] = useState(false);
  // Run and navigation wait until the dialog has closed and restored focus, so the main window's Stop stays reachable.
  const then = useRef<(()=>void)|null>(null);
  useEffect(() => {
    if (open) return;
    const action = then.current;
    then.current = null;
    action?.();
  }, [open]);
  function leave(action:()=>void) {
    if (!open) {action(); return;}
    then.current = action;
    setOpen(false);
  }
  function show() {
    setOpen(true);
    // Opening is the explicit request to read the saved setup; a failure waits for an explicit Load.
    if (!state && !locked && !loadBlocked && !loading) handlers.reload();
  }
  const bound = state?.bound;
  const candidate = state?.context.functions.find(item => functionKey(item) === state.selected);
  const args = state?.unit === 'function' ? trialArguments(state.arguments, candidate?.required_arguments) : null;
  const diagnostics = error ? trialDiagnostics(error) : [];
  const setupBlock = !state || !facts ? s.load : !state.profileChoice ? s.profileRequired
    : state.unit === 'function' && !candidate ? state.context.functions.length === 0 ? s.noFunctions : s.chooseFunction
    : args?.error ? s.argumentErrors[args.error]
    : !facts.bound ? t.run.staleSchema : facts.numericErrors ? messages[locale].app.invalidFields
    : facts.descriptorError ?? facts.startBlock;
  const reason = block ?? (dirty && bound?.lane === 'native' && setupBlock === messages[locale].app.nativeApproval ? null : setupBlock);
  const status = `${t.status.savedPackage} · ${validation === 'running' ? t.authoring.validating : t.status[validation]}`;
  const choice = state?.profileChoice ?? '';
  const profile = choice === '' ? s.chooseProfile : choice === 'values' ? s.explicitValues
    : choice.startsWith('profile:') ? bound?.profiles.find(item => item.id === choice.slice(8))?.name ?? choice.slice(8)
    : `${t.run.preset} · ${choice.slice(7)}`;
  const activity = open ? authoring(leave) : null;
  return <>
    <button id="trial-setup-open" type="button" aria-haspopup="dialog" aria-label={s.openSetup} aria-describedby="package-validation-status"
      title={status} onClick={show}>
      <ValidationStatusIcon id="package-validation-status" label={t.status.savedPackage} state={validation}/>{s.openSetup}
    </button>
    <Modal id="trial-setup-dialog" className="trial-dialog" open={open} onCancel={() => setOpen(false)} labelledBy="trial-setup-heading"
      initialFocus="#trial-setup-heading" returnFocus={() => document.getElementById('trial-setup-open')}>
      <div className="dialog-header">
        <div className="trial-dialog-title"><h2 id="trial-setup-heading" tabIndex={-1}>{s.heading}</h2>
          <InfoHint id="trial-help" label={`${t.recognition.previewHelpButton} · ${s.heading}`} hint={s.help}/></div>
        <button id="trial-setup-close" type="button" className="icon" aria-label={t.common.close} title={t.common.close} onClick={() => setOpen(false)}>×</button>
      </div>
      {strip}
      <div className="dialog-body">
        <div className="button-row trial-links">
          <button id="trial-configure" type="button" className="link" onClick={() => leave(handlers.setup)}>{s.configure}</button>
        </div>
        {state && <>
          <div className="field"><label htmlFor="trial-unit">{s.unit}</label>
            <Select id="trial-unit" value={state.unit} disabled={locked} onChange={unit => handlers.change(current => ({...current, unit:unit === 'function' ? 'function' : 'package'}))}
              options={[{value:'package', label:s.package}, {value:'function', label:s.function}]}/></div>
          {state.unit === 'function' && <>
            <div className="field"><label htmlFor="trial-function">{s.function}</label>
              <Select id="trial-function" value={state.selected ?? ''} disabled={locked} aria-describedby={candidate ? 'trial-signature' : undefined}
                onChange={selected => handlers.change(current => ({...current, selected:selected || null}))}
                options={[{value:'', label:s.chooseFunction, disabled:true}, ...state.context.functions.map(item => {
                  const label = `${item.module} · ${item.function} · ${item.line}:${item.column}`;
                  return {value:functionKey(item), label, title:label};
                })]}/>
              {candidate && <p id="trial-signature" className="trial-signature mono">{candidate.module}:{candidate.line}:{candidate.column} · {candidate.signature}</p>}</div>
            <div className="field"><label htmlFor="trial-arguments">{s.arguments}</label>
              <textarea id="trial-arguments" value={state.arguments} disabled={locked} spellCheck={false} aria-describedby="trial-arguments-help" aria-invalid={args?.error !== null}
                onChange={event => handlers.change(current => ({...current, arguments:event.target.value}))}/>
              <p id="trial-arguments-help" className="field-help">{args?.error ? s.argumentErrors[args.error] : s.argumentsHelp}</p></div>
          </>}
          <dl id="trial-summary" className="run-identity">
            <dt>{t.run.nextRunProfile}</dt><dd>{profile}</dd>
            <dt>{t.run.executionMode}</dt><dd>{t.lane(state.bound.lane)}</dd>
          </dl>
        </>}
        <div className="button-row trial-links">
          {hasResult && <>
            <button id="trial-results" type="button" className="link" onClick={() => leave(handlers.results)}>{s.openResults}</button>
            <button id="trial-logs" type="button" className="link" onClick={() => leave(handlers.logs)}>{s.logs}</button>
          </>}
          {state && <button id="trial-reload" type="button" disabled={locked || loadBlocked} onClick={handlers.reload}>{s.refresh}</button>}
        </div>
        {state && dirty && <p className="field-help">{s.saveHelp}{bound?.lane === 'native' && <> {s.nativeSaveHelp}</>}</p>}
        {notice && <p role="status" className="inline-warning">{notice}</p>}
        {error && <FaultMessage title={s.heading} value={diagnostics.length ? {...error, context:null} : error}/>}
        {diagnostics.length > 0 && <section aria-label={s.diagnostics}>
          {errorSource && <p className="field-help">{s.diagnosticRevision} <code>{errorSource.revision}</code></p>}
          {errorSource?.stale && <p className="inline-warning">{s.sourceNewer}</p>}
          <p className="field-help">{s.diagnosticLimit}</p>
          <ol id="trial-diagnostics" className="diagnostic-list">{diagnostics.map((item, index) => {
            const location = trialDiagnosticLocation(item);
            const where = location ? t.authoring.location(location.path, location.line, location.column) : null;
            const available = location && errorSource?.paths.includes(location.path) && onDiagnostic;
            return <li key={index}>
              {where && available
                ? <button type="button" className="link diagnostic-location" data-path={location.path}
                  aria-label={`${t.authoring.goTo(where)}${errorSource?.stale ? ` · ${s.diagnosticObsolete}` : ''}`}
                  onClick={() => leave(() => onDiagnostic?.(item))}>{where}</button>
                : <span className="muted">{where === null ? t.authoring.unlocated : t.authoring.diagnosticUnavailable(where)}</span>}
              {where && errorSource?.stale && <span className="tag stale"> {s.diagnosticObsolete}</span>}
              <strong> {item.category}</strong> <span>{item.message}</span>
            </li>;
          })}</ol>
        </section>}
      </div>
      <div className="dialog-footer">
        <span id="trial-block" role="status">{loading ? s.loading : state ? reason ?? '' : block ?? ''}</span>
        {activity && <AuthoringFooterStatus key={activity.activity.owner.token} {...activity} idPrefix="trial-authoring"/>}
        {state
          ? <button id="trial-run" type="button" className="primary" disabled={locked || reason !== null} title={reason ?? undefined}
            onClick={() => leave(handlers.run)}>{hasResult ? dirty ? s.saveRerun : s.rerun : dirty ? s.saveRun : s.run}</button>
          : <button id="trial-load" type="button" className="primary" disabled={locked || loadBlocked || loading} onClick={handlers.reload}>{s.load}</button>}
      </div>
    </Modal>
  </>;
}

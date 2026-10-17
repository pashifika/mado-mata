import type {ReactNode} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {functionKey, trialArguments, trialDiagnosticLocation, trialDiagnostics} from '../edit-trial.ts';
import type {EditTrialState} from '../edit-trial.ts';
import {chooseLane, editDraft, newDraft, selectProfile} from '../workspace.ts';
import type {Bound, Derived, NativeConsent} from '../workspace.ts';
import type {Fault, NativeCapability, OcrEnvironment} from '../types.ts';
import {HelpTrigger} from './ContextualHelp.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import NativeReview from './NativeReview.tsx';
import SchemaForm from './SchemaForm.tsx';
import Select from './Select.tsx';
import TargetPanel from './TargetPanel.tsx';
import type {TargetHandlers} from './TargetPanel.tsx';

export interface EditTrialHandlers {
  change:(update:(state:EditTrialState)=>EditTrialState)=>void;
  bound:(update:(bound:Bound)=>Bound)=>void;
  reload:()=>void; run:()=>void;
  native:{edit:(field:'operation'|'postcondition'|'workflowSeconds', value:string)=>void; approve:(field:NativeConsent, value:boolean)=>void};
  target:TargetHandlers;
  logs:()=>void;
}

export default function EditTrial({state, facts, handlers, locked, loadBlocked, block, dirty, error, errorSource, onDiagnostic, notice, nativeCapability, nativeError, environment, result, profileRepair}: {
  state:EditTrialState|null; facts:Derived|null; handlers:EditTrialHandlers; locked:boolean; loadBlocked:boolean; block:string|null; dirty:boolean;
  error:Fault|null; errorSource?:{revision:string; stale:boolean; paths:readonly string[]}|null; onDiagnostic?:(diagnostic:Fault)=>void;
  notice:string|null; nativeCapability:NativeCapability|null; nativeError:Fault|null; environment:OcrEnvironment|null; result:ReactNode; profileRepair:ReactNode;
}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const s = t.scriptTrial;
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
  function change(update:(state:EditTrialState)=>EditTrialState) {handlers.change(update);}
  return <section className="script-trial" aria-labelledby="script-trial-heading">
    <div className="trial-strip">
      <h2 id="script-trial-heading">{s.heading}<HelpTrigger title={s.heading} hint={s.help}><p>{s.help}</p><p>{s.saveHelp}</p></HelpTrigger></h2>
      {state && <>
        <label className="visually-hidden" htmlFor="trial-unit">{s.unit}</label>
        <Select id="trial-unit" value={state.unit} disabled={locked} onChange={unit => change(current => ({...current, unit:unit === 'function' ? 'function' : 'package'}))}
          options={[{value:'package', label:s.package}, {value:'function', label:s.function}]}/>
        {state.unit === 'function' && <><label className="visually-hidden" htmlFor="trial-function">{s.function}</label>
          <Select id="trial-function" className="trial-function-select" value={state.selected ?? ''} disabled={locked} onChange={selected => change(current => ({...current, selected:selected || null}))}
            options={[{value:'', label:s.chooseFunction, disabled:true}, ...state.context.functions.map(item => ({value:functionKey(item), label:`${item.module} · ${item.function} · ${item.line}:${item.column}`}))]}/></>}
        <button id="trial-run" className="primary" type="button" disabled={locked || reason !== null} title={reason ?? undefined} onClick={handlers.run}>{dirty ? s.saveRun : s.run}</button>
        {result && <button id="trial-rerun" type="button" disabled={locked || reason !== null} title={reason ?? undefined} onClick={handlers.run}>{dirty ? s.saveRerun : s.rerun}</button>}
      </>}
      {!state && <button id="trial-load" type="button" disabled={locked || loadBlocked} onClick={handlers.reload}>{s.load}</button>}
    </div>
    {candidate && state?.unit === 'function' && <p className="trial-signature mono">{candidate.module}:{candidate.line}:{candidate.column} · {candidate.signature}</p>}
    {dirty && <p className="field-help">{s.saveHelp}{bound?.lane === 'native' && <> {s.nativeSaveHelp}</>}</p>}
    {reason && <p id="trial-block" className="field-help">{reason}</p>}
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
              onClick={() => onDiagnostic?.(item)}>{where}</button>
            : <span className="muted">{where === null ? t.authoring.unlocated : t.authoring.diagnosticUnavailable(where)}</span>}
          {where && errorSource?.stale && <span className="tag stale"> {s.diagnosticObsolete}</span>}
          <strong> {item.category}</strong> <span>{item.message}</span>
        </li>;
      })}</ol>
    </section>}
    {state && bound && facts && <details id="trial-setup" className="trial-setup"><summary>{s.setup}</summary>
      <div className="button-row"><button id="trial-reload" type="button" disabled={locked || loadBlocked} onClick={handlers.reload}>{s.refresh}</button></div>
      {state.unit === 'function' && <div className="field"><label htmlFor="trial-arguments">{s.arguments}</label>
        <textarea id="trial-arguments" value={state.arguments} disabled={locked} spellCheck={false} aria-describedby="trial-arguments-help" aria-invalid={args?.error !== null}
          onChange={event => change(current => ({...current, arguments:event.target.value}))}/>
        <p id="trial-arguments-help" className="field-help">{args?.error ? s.argumentErrors[args.error] : s.argumentsHelp}</p></div>}
      <div className="run-grid"><section className="panel"><div className="panel-body">
        <p className="field-help">{s.profileHelp}</p>
        {bound.profilesError && <FaultMessage title={t.run.profilesError} value={bound.profilesError}/>}
        <div className="field"><label htmlFor="trial-profile">{t.run.nextRunProfile}</label>
          <Select id="trial-profile" value={state.profileChoice} disabled={locked} onChange={choice => change(current => {
            const next = choice.startsWith('profile:') ? selectProfile(current.bound, choice.slice(8))
              : newDraft(current.bound, choice.startsWith('preset:') ? choice.slice(7) : '');
            return {...current, bound:next, profileChoice:choice};
          })} options={[{value:'', label:s.chooseProfile, disabled:true}, {value:'values', label:s.explicitValues},
            ...bound.profiles.map(profile => ({value:`profile:${profile.id}`, label:profile.name})),
            ...Object.keys(bound.package.profiles).map(name => ({value:`preset:${name}`, label:`${t.run.preset} · ${name}`}))]}/></div>
        {!facts.bound && <p className="inline-warning">{t.run.staleSchema}</p>}
        <fieldset disabled={locked}><legend>{t.run.options}</legend>
          <SchemaForm idPrefix="trial-option" schema={bound.package.schema} value={bound.draft} errors={facts.parsed.errors}
            onChange={draft => handlers.bound(current => editDraft(current, draft))}/></fieldset>
        <div className="field"><label htmlFor="trial-lane">{t.run.executionMode}</label>
          <Select id="trial-lane" value={bound.lane} disabled={locked} onChange={lane => handlers.bound(current => chooseLane(current, lane))}
            options={[{value:'controlled', label:t.run.controlled}, {value:'replay', label:t.run.replay},
              {value:'native', label:nativeCapability ? t.run.native : t.run.nativeUnavailable, disabled:nativeCapability === null}]}/></div>
        {bound.lane === 'replay' && <div className="field"><label htmlFor="trial-descriptor">{t.run.descriptor}</label>
          <input id="trial-descriptor" value={bound.descriptorPath} disabled={locked} spellCheck={false}
            onChange={event => handlers.bound(current => ({...current, descriptorPath:event.target.value}))}/></div>}
        {nativeError && <FaultMessage title={t.run.nativeCapabilityError} value={nativeError}/>}
        {facts.native && <>
          <dl className="run-identity">
            <dt>{t.authoring.revision}</dt><dd><code>{state.context.revision}</code></dd>
            <dt>{s.unit}</dt><dd>{state.unit === 'function' ? candidate ? `${candidate.module} · ${candidate.function}` : s.chooseFunction : s.package}</dd>
          </dl>
          {state.unit === 'function' && <details><summary>{s.arguments}</summary><pre>{state.arguments}</pre></details>}
          <NativeReview bound={bound} native={facts.native} selectedProfile={facts.selectedProfile} valuesDirty={facts.valuesDirty} locked={locked}
            nativeCapability={nativeCapability} environment={environment} attachOnly={state.unit === 'function'}
            reviewBlocked={dirty || block !== null || !state.profileChoice || !facts.bound || facts.numericErrors
              || (state.unit === 'function' && (!candidate || args?.error !== null))} handlers={handlers}/>
        </>}
      </div></section>
      <TargetPanel state={bound.target} handlers={handlers.target} locked={locked} active={locked} pickerBusy={bound.target.picker !== null}/></div>
      {profileRepair}
    </details>}
    {result && <details id="trial-result" className="trial-result"><summary>{s.result}</summary>
      <button type="button" onClick={handlers.logs}>{s.logs}</button>{result}</details>}
  </section>;
}

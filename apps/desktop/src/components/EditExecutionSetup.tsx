import type {ReactNode} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {functionKey, trialArguments} from '../edit-trial.ts';
import type {EditTrialState} from '../edit-trial.ts';
import {newDraft, selectProfile} from '../workspace.ts';
import type {Derived} from '../workspace.ts';
import type {Fault, NativeCapability, OcrEnvironment} from '../types.ts';
import type {EditTrialHandlers} from './EditTrial.tsx';
import ExecutionFields from './ExecutionFields.tsx';
import NativeReview from './NativeReview.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import Select from './Select.tsx';
import TargetPanel from './TargetPanel.tsx';

export default function EditExecutionSetup({state, facts, handlers, locked, loadBlocked, loading, block, dirty, error, nativeCapability, nativeError, environment, profileRepair, onReturn}: {
  state:EditTrialState|null; facts:Derived|null; handlers:EditTrialHandlers;
  locked:boolean; loadBlocked:boolean; loading:boolean; block:string|null; dirty:boolean; error:Fault|null;
  nativeCapability:NativeCapability|null; nativeError:Fault|null; environment:OcrEnvironment|null;
  profileRepair:ReactNode; onReturn:()=>void;
}) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const s = t.scriptTrial;
  const bound = state?.bound;
  const candidate = state?.context.functions.find(item => functionKey(item) === state.selected);
  const args = state?.unit === 'function' ? trialArguments(state.arguments, candidate?.required_arguments) : null;
  const setupBlock = !state || !facts ? s.load : !state.profileChoice ? s.profileRequired
    : state.unit === 'function' && !candidate ? state.context.functions.length === 0 ? s.noFunctions : s.chooseFunction
    : args?.error ? s.argumentErrors[args.error]
    : !facts.bound ? t.run.staleSchema : facts.numericErrors ? messages[locale].app.invalidFields
    : facts.descriptorError ?? facts.startBlock;
  const reason = block ?? (dirty && bound?.lane === 'native' && setupBlock === messages[locale].app.nativeApproval ? null : setupBlock);
  const fieldsLocked = locked || loading;
  return <>
    <div className="page-heading"><div>
      <span className="eyebrow">{s.heading}</span>
      <h1 id="edit-execution-setup-heading" tabIndex={-1}>{s.setup}</h1>
      <p>{s.setupHelp}</p>
    </div><div className="actions">
      <button id="trial-return-edit" type="button" onClick={onReturn}>{s.returnToEdit}</button>
    </div></div>
    <div className="button-row">
      <button id={state ? 'trial-reload' : 'trial-load'} type="button" disabled={fieldsLocked || loadBlocked} onClick={handlers.reload}>
        {loading ? s.loading : state ? s.refresh : s.load}
      </button>
      <button type="button" onClick={handlers.results}>{s.openResults}</button>
    </div>
    {loading && <p role="status" className="muted">{s.loading}</p>}
    {error && <FaultMessage title={s.setup} value={error}/>}
    {reason && <p id="trial-setup-block" className="inline-warning">{reason}</p>}
    {dirty && <p className="field-help">{s.saveHelp}{bound?.lane === 'native' && <> {s.nativeSaveHelp}</>}</p>}
    {state && bound && facts && <>
      <div className="run-grid">
        <section className="panel" aria-labelledby="trial-configuration-heading">
          <div className="panel-heading"><h2 id="trial-configuration-heading">{t.run.configuration}</h2></div>
          <div className="panel-body">
            <dl className="run-identity">
              <dt>{t.distribution.packageId}</dt><dd><code>{bound.package.package_id}</code></dd>
              <dt>{t.authoring.revision}</dt><dd><code>{state.context.revision}</code></dd>
              <dt>{s.unit}</dt><dd>{state.unit === 'function' ? candidate ? `${candidate.module} · ${candidate.function}` : s.chooseFunction : s.package}</dd>
            </dl>
            {state.unit === 'function' && <>
              {candidate && <p className="trial-signature mono">{candidate.module}:{candidate.line}:{candidate.column} · {candidate.signature}</p>}
              <details><summary>{s.arguments}</summary><pre>{state.arguments}</pre></details>
            </>}
            <p className="field-help">{s.profileHelp}</p>
            {bound.profilesError && <FaultMessage title={t.run.profilesError} value={bound.profilesError}/>}
            <div className="field"><label htmlFor="trial-profile">{t.run.nextRunProfile}</label>
              <Select id="trial-profile" value={state.profileChoice} disabled={fieldsLocked} onChange={choice => handlers.change(current => {
                const next = choice.startsWith('profile:') ? selectProfile(current.bound, choice.slice(8))
                  : newDraft(current.bound, choice.startsWith('preset:') ? choice.slice(7) : '');
                return {...current, bound:next, profileChoice:choice};
              })} options={[{value:'', label:s.chooseProfile, disabled:true}, {value:'values', label:s.explicitValues},
                ...bound.profiles.map(profile => ({value:`profile:${profile.id}`, label:profile.name})),
                ...Object.keys(bound.package.profiles).map(name => ({value:`preset:${name}`, label:`${t.run.preset} · ${name}`}))]}/>
            </div>
            {!facts.bound && <p className="inline-warning">{t.run.staleSchema}</p>}
            <ExecutionFields bound={bound} facts={facts} locked={fieldsLocked} nativeCapability={nativeCapability} change={handlers.bound} trial>
              {nativeError && <FaultMessage title={t.run.nativeCapabilityError} value={nativeError}/>}
              {facts.native && <NativeReview bound={bound} native={facts.native} selectedProfile={facts.selectedProfile} valuesDirty={facts.valuesDirty}
                locked={fieldsLocked} nativeCapability={nativeCapability} environment={environment} attachOnly={state.unit === 'function'}
                reviewBlocked={dirty || block !== null || !state.profileChoice || !facts.bound || facts.numericErrors
                  || (state.unit === 'function' && (!candidate || args?.error !== null))} handlers={handlers}/>}
            </ExecutionFields>
          </div>
        </section>
        <TargetPanel state={bound.target} handlers={handlers.target} locked={fieldsLocked} active={fieldsLocked} pickerBusy={bound.target.picker !== null}/>
      </div>
      {profileRepair}
    </>}
  </>;
}

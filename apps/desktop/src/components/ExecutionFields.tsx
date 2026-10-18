import type {ReactNode} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {DESCRIPTOR_LIMIT, chooseLane, editDraft} from '../workspace.ts';
import type {Bound, Derived} from '../workspace.ts';
import type {NativeCapability} from '../types.ts';
import {HelpTrigger} from './ContextualHelp.tsx';
import SchemaForm from './SchemaForm.tsx';
import Select from './Select.tsx';

export default function ExecutionFields({bound, facts, locked, nativeCapability, change, trial = false, children}: {
  bound:Bound; facts:Pick<Derived, 'parsed'|'descriptorError'>; locked:boolean; nativeCapability:NativeCapability|null;
  change:(update:(bound:Bound)=>Bound)=>void; trial?:boolean; children?:ReactNode;
}) {
  const t = messages[useLocale()].ui;
  const laneId = trial ? 'trial-lane' : 'lane';
  const scenarioId = 'scenario';
  const descriptorId = trial ? 'trial-descriptor' : 'descriptor-path';
  return <>
    <h3>{t.run.options}<HelpTrigger title={t.run.options} hint={t.run.optionsHint}>{t.run.optionsHelp}</HelpTrigger></h3>
    <fieldset className="execution-options" disabled={locked}>
      <legend className="visually-hidden">{t.run.options}</legend>
      <SchemaForm idPrefix={trial ? 'trial-option' : 'option'} schema={bound.package.schema} value={bound.draft} errors={facts.parsed.errors}
        onChange={draft => change(item => editDraft(item, draft))}/>
    </fieldset>
    <h3>{t.run.executionMode}</h3>
    <div className={trial ? undefined : 'two-col'}>
      <div className="field"><label htmlFor={laneId}>{t.run.executionMode}</label>
        <Select id={laneId} value={bound.lane} disabled={locked} onChange={lane => change(item => chooseLane(item, lane))}
          options={[{value:'controlled', label:t.run.controlled}, {value:'replay', label:t.run.replay},
            {value:'native', label:nativeCapability ? t.run.native : t.run.nativeUnavailable, disabled:nativeCapability === null}]}/></div>
      {!trial && <div className="field"><label htmlFor={scenarioId}>{t.run.scenario}</label>
        <Select id={scenarioId} value={bound.lane === 'controlled' ? bound.scenario : 'workflow'} disabled={locked || bound.lane !== 'controlled'}
          onChange={scenario => change(item => ({...item, scenario}))}
          options={[{value:'workflow', label:t.run.workflow}, {value:'held-work', label:t.run.heldWork}, {value:'no-match', label:t.run.noMatch}]}/></div>}
    </div>
    {children}
    {(!trial || bound.lane === 'replay') && <div className="field">
      <div className="field-heading"><label htmlFor={descriptorId}>{t.run.descriptor}</label>
        <HelpTrigger title={t.run.descriptor} hint={t.run.descriptorHint}>{t.run.descriptorHelp(DESCRIPTOR_LIMIT)}</HelpTrigger></div>
      <input id={descriptorId} type="text" value={bound.descriptorPath} disabled={locked} spellCheck={false} aria-invalid={facts.descriptorError !== null}
        placeholder={t.run.descriptorPlaceholder} onChange={event => change(item => ({...item, descriptorPath:event.target.value}))}/>
      {facts.descriptorError && <p className="field-error">{facts.descriptorError}</p>}
    </div>}
  </>;
}

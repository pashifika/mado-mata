import {Fragment} from 'react';
import type {ReactNode} from 'react';
import {HelpTrigger} from './ContextualHelp.tsx';
import type {Bound, NativeConsent, NativeFacts} from '../workspace.ts';
import type {NativeCapability, OcrEnvironment} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export default function NativeReview({bound, native, selectedProfile, valuesDirty, locked, nativeCapability, environment, handlers}: {
  bound:Bound; native:NativeFacts; selectedProfile:{name:string; id:string}|undefined; valuesDirty:boolean;
  locked:boolean; nativeCapability:NativeCapability|null; environment:OcrEnvironment|null;
  handlers:{native:{edit:(field:'operation'|'postcondition'|'workflowSeconds', value:string)=>void; approve:(field:NativeConsent, value:boolean)=>void}};
}) {
  const t = messages[useLocale()].ui;
  const nativeBinding = native.binding;
  const nativeInput = nativeBinding?.configuration.input ?? null;
  const recipe = native?.recipe ?? null;
  // Launch recipe validity is conditional on host-confirmed absence, not a gate on attaching.
  const approvalOpen = (native.block === null || native.block === 'nativeApproval') && !locked;
  // Invalid Workflow text has no effective duration; never display a clamped or fallback budget.
  const nativeLimits = nativeCapability?.default_limits ?? null;
  const selectedLimits = native?.limits ?? null;
  const nativeBudgetRows = nativeLimits && [
    [t.run.nativeStartup, nativeLimits.startup_ms], [t.run.nativeReadiness, nativeLimits.readiness_ms], [t.run.nativeWorkflow, selectedLimits?.workflow_ms ?? null],
  ] as const;
  const nativeDuration = selectedLimits ? (1 + (native.recovery ? 1 : 0)) * (selectedLimits.startup_ms + selectedLimits.readiness_ms + selectedLimits.workflow_ms) + (native.recovery ? 3000 : 0) : null;
  const nativeLimitRows = nativeLimits && [
    [t.run.nativeFrames, String(nativeLimits.max_frames)],
    [t.run.nativeWait, `${nativeLimits.wait_ms} ms`], [t.run.nativeInterval, `${nativeLimits.interval_ms} ms`],
    [t.run.nativeActions, String(nativeLimits.max_actions)], [t.run.nativeCleanup, `${nativeLimits.cleanup_ms} ms`],
    [t.run.nativeContainment, `${nativeLimits.containment_ms} ms`],
  ];
  const identity:{term:string; value:ReactNode; id?:string; mono?:boolean}[] = [
    {term:t.run.nativePackage, value:`${bound.package.package_id} / ${selectedProfile && !valuesDirty ? `${selectedProfile.name} · ${selectedProfile.id}` : t.run.profileDraft}`},
    {term:t.common.environment, value:environment ? `${environment.profile} · ${environment.model} · ${environment.language} · ${environment.provider} · ${environment.runtime_profile}` : t.common.unconfigured},
    {term:t.run.nativeApplication, value:nativeBinding?.configuration.game.path ?? t.common.none, mono:true},
    {term:t.target.windowTitle, value:nativeBinding?.configuration.window_title ?? t.common.none},
    {term:t.target.route, value:nativeInput ? nativeInput.route === 'system' ? t.target.system : t.target.processDirected : t.common.none, id:'native-route'},
    {term:t.target.focus, value:nativeInput ? nativeInput.focus === 'preserve' ? t.target.preserve : t.target.requireFocused : t.common.none, id:'native-focus'},
    {term:t.target.pointerMode, value:nativeInput?.pointer_mode === 'core_graphics' ? t.target.coreGraphics : nativeInput?.pointer_mode === 'appkit_background' ? t.target.appkitBackground : t.common.none, id:'native-pointer'},
    {term:t.target.clickHold, value:nativeInput ? `${nativeInput.click_hold_ms} ms` : t.common.none},
    {term:t.run.nativeBinding, value:nativeBinding && bound.target.view ? <><code>{nativeBinding.id}</code> · {t.target.revision(bound.target.view.record.revision)}</> : t.common.none},
  ];
  const rows = (items:typeof identity) => items.map(item => <Fragment key={item.term}>
    <dt>{item.term}</dt><dd id={item.id} className={item.mono ? 'mono' : undefined}>{item.value}</dd>
  </Fragment>);
  const help = <p className="field-help">{t.run.nativeHelp}</p>;
  const recipeSection = recipe && <><span className="eyebrow">{t.run.nativeRecipe}</span>
    <dl className="run-identity" id="native-recipe">
      <dt>{t.run.nativeRecipient}</dt><dd id="native-recipient">{recipe.recipient === 'game' ? t.run.nativeRecipientGame
        : <>{t.run.nativeRecipientLauncher} · {recipe.location.kind === 'bundle' ? t.target.bundle : t.target.executable} · <span className="mono">{recipe.location.path}</span></>}</dd>
      <dt>{t.run.nativeArguments}</dt><dd>{recipe.arguments.length === 0 ? t.run.nativeNoArguments
        : <ol id="native-arguments" className="native-arguments">{recipe.arguments.map((value, index) => <li key={index}><code>{JSON.stringify(value)}</code></li>)}</ol>}</dd>
      <dt>{t.run.nativeDirectory}</dt><dd id="native-directory" className={recipe.directory === 'refused' ? 'field-error' : undefined}>
        {recipe.workingDirectory !== null && <><code>{recipe.workingDirectory}</code> · </>}{t.run.nativeDirectories[recipe.directory]}</dd>
    </dl>
    {recipe.recipient === 'launcher' && <p className="field-help">{t.run.nativeForwarding}</p>}</>;
  const workflowField = nativeCapability && <div className="field"><label htmlFor="native-workflow-seconds">{t.run.nativeWorkflowSeconds}</label>
    <input id="native-workflow-seconds" type="text" inputMode="numeric" value={native.workflowSeconds} disabled={locked} spellCheck={false}
      aria-invalid={native.workflowError} aria-describedby={`native-workflow-help${native.workflowError ? ' native-workflow-error' : ''}`}
      onChange={event => handlers.native.edit('workflowSeconds', event.target.value)}/>
    {native.workflowError && <p className="field-error" id="native-workflow-error">{t.run.nativeWorkflowError(nativeCapability.max_workflow_ms / 1000)}</p>}
    <p className="field-help" id="native-workflow-help">{t.run.nativeWorkflowHelp(nativeCapability.default_limits.workflow_ms / 1000, nativeCapability.max_workflow_ms / 1000)}</p></div>;
  const budgets = nativeBudgetRows && <><span className="eyebrow">{t.run.nativeBudgets}<HelpTrigger title={t.run.nativeBudgets} hint={t.run.nativeBudgetHint}>
    <p>{t.run.nativeBudgetHelp}</p><p>{t.run.nativeStartupHelp}</p>
  </HelpTrigger></span>
    <dl className="run-identity" id="native-budgets">{nativeBudgetRows.map(([term, value]) => <Fragment key={term}><dt>{term}</dt><dd>{value === null ? t.common.none : `${value} ms`}</dd></Fragment>)}</dl>
    <dl className="run-identity"><dt>{t.run.nativeDuration}</dt><dd id="native-duration">{nativeDuration === null ? t.common.none : t.run.nativeEnvelope(nativeDuration)}</dd></dl></>;
  const limits = nativeLimitRows && <><span className="eyebrow">{t.run.nativeLimits}</span>
    <dl className="run-identity" id="native-limits">{nativeLimitRows.map(([term, value]) => <Fragment key={term}><dt>{term}</dt><dd>{value}</dd></Fragment>)}</dl></>;
  const statements = <>
    <div className="field"><label htmlFor="native-operation">{t.run.nativeOperation}</label>
      <input id="native-operation" type="text" value={bound.native.operation} disabled={locked} spellCheck={false} aria-invalid={bound.native.operation !== '' && native.operationError !== null}
        onChange={event => handlers.native.edit('operation', event.target.value)}/>
      {bound.native.operation !== '' && native.operationError && <p className="field-error">{t.run.nativeTextErrors[native.operationError]}</p>}
      <p className="field-help">{t.run.nativeOperationHelp}</p></div>
    <div className="field"><label htmlFor="native-postcondition">{t.run.nativePostcondition}</label>
      <input id="native-postcondition" type="text" value={bound.native.postcondition} disabled={locked} spellCheck={false} aria-invalid={bound.native.postcondition !== '' && native.postconditionError !== null}
        onChange={event => handlers.native.edit('postcondition', event.target.value)}/>
      {bound.native.postcondition !== '' && native.postconditionError && <p className="field-error">{t.run.nativeTextErrors[native.postconditionError]}</p>}
      <p className="field-help">{t.run.nativePostconditionHelp}</p></div>
  </>;
  const captureInput = <>
    <label className="checkbox-label"><input id="native-capture-consent" type="checkbox" checked={native.capture} disabled={!approvalOpen}
      onChange={event => handlers.native.approve('capture', event.target.checked)}/>{t.run.nativeCapture}</label>
    <label className="checkbox-label"><input id="native-input-consent" type="checkbox" checked={native.input} disabled={!approvalOpen}
      onChange={event => handlers.native.approve('input', event.target.checked)}/>{t.run.nativeInput}</label>
  </>;
  const launchRecovery = <><label className="checkbox-label"><input id="native-launch-consent" type="checkbox" checked={native.launch} disabled={!approvalOpen}
    onChange={event => handlers.native.approve('launch', event.target.checked)}/>{t.run.nativeLaunch}</label>
    <p className="field-help" id="native-launch-help">{t.run.nativeLaunchHelp}</p>
    <label className="checkbox-label"><input id="native-recovery-consent" type="checkbox" checked={native.recovery} disabled={!approvalOpen || !native.launch}
      aria-describedby="native-recovery-help" onChange={event => handlers.native.approve('recovery', event.target.checked)}/>{t.run.nativeRecovery}</label>
    <p className="field-help" id="native-recovery-help">{t.run.nativeRecoveryHelp}</p></>;
  const approvalHelp = <p className="field-help">{t.run.nativeApprovalHelp}</p>;
  return <div id="native-review">
    <h3>{t.run.nativeHeading}</h3>
    {help}
    <dl className="run-identity" id="native-target">{rows(identity)}</dl>
    {recipeSection}
    {workflowField}
    {budgets}
    {limits}
    {statements}
    <div className="field">{captureInput}{launchRecovery}{approvalHelp}</div>
  </div>;
}

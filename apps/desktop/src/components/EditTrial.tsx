import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {functionKey, trialArguments, trialDiagnosticLocation, trialDiagnostics} from '../edit-trial.ts';
import type {EditTrialState, TrialConfirmation} from '../edit-trial.ts';
import type {PackageValidation} from '../status.ts';
import {chooseLane} from '../workspace.ts';
import type {Bound, Derived} from '../workspace.ts';
import type {Fault, NativeCapability, OcrEnvironment} from '../types.ts';
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
  confirm:(capture:TrialConfirmation)=>void; cancelConfirmation:()=>void;
  native:{edit:(value:string)=>void; approve:(field:'launch'|'recovery', value:boolean)=>void};
  target:TargetHandlers;
  logs:()=>void;
  // Optional advanced setup in Run, focused on its target or values section; the saved OCR environment in Settings;
  // and this owner's results in Execution.
  setup:(section?:'target'|'values')=>void; environment:()=>void; results:()=>void;
}

// Edit's Script execution entry: a Run… trigger beside Validate and one dialog over the same trial state. Routine
// execution, prerequisites and Native review finish here; Run holds only optional advanced setup.
export default function EditTrial({state, facts, confirmation, stale, open, onOpenChange, nativeCapability, nativeError, environment, handlers, locked, loadBlocked, loading, block, dirty, validation, error, errorSource, onDiagnostic, notice, hasResult, strip, authoring}: {
  state:EditTrialState|null; facts:Derived|null; confirmation:TrialConfirmation|null; stale:boolean;
  // Owner-bound visibility: the parent reopens the dialog when advanced setup or OCR environment settings return.
  open:boolean; onOpenChange:(open:boolean)=>void;
  nativeCapability:NativeCapability|null; nativeError:Fault|null; environment:OcrEnvironment|null;
  handlers:EditTrialHandlers; locked:boolean; loadBlocked:boolean; loading:boolean; block:string|null; dirty:boolean;
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
  const app = messages[locale].app;
  const s = t.scriptTrial;
  // The function whose arguments the operator expanded; another selection starts collapsed again.
  const [argumentsOpen, setArgumentsOpen] = useState<string|null>(null);
  // Navigation waits for focus restoration. Execution keeps Stop in this dialog until final admission.
  const then = useRef<(()=>void)|null>(null);
  const confirmationWasOpen = useRef(false);
  useEffect(() => {
    if (open) return;
    const action = then.current;
    then.current = null;
    action?.();
  }, [open]);
  useEffect(() => {
    if (confirmation) document.getElementById('trial-confirm-heading')?.focus();
    else if (confirmationWasOpen.current) document.getElementById(loading ? 'trial-setup-heading' : 'trial-run')?.focus();
    confirmationWasOpen.current = confirmation !== null;
  }, [confirmation]);
  function leave(action:()=>void) {
    if (!open) {action(); return;}
    then.current = action;
    onOpenChange(false);
  }
  function show() {
    onOpenChange(true);
    // Opening is the explicit request to read the saved setup; a failure waits for an explicit Load.
    if (!state && !locked && !loadBlocked && !loading) handlers.reload();
  }
  const bound = state?.bound;
  const lane = bound?.lane;
  const native = facts?.native ?? null;
  const candidate = state?.unit === 'function' ? state.context.functions.find(item => functionKey(item) === state.selected) : undefined;
  const args = state?.unit === 'function' ? trialArguments(state.arguments, candidate?.required_arguments) : null;
  const diagnostics = error ? trialDiagnostics(error) : [];
  const valuesBlock = !state || !facts ? null : !state.profileChoice ? s.profileRequired
    : !facts.bound ? t.run.staleSchema : facts.numericErrors ? app.invalidFields : null;
  const setupBlock = !state || !facts ? s.load : !state.profileChoice ? s.profileRequired
    : state.unit === 'function' && !candidate ? state.context.functions.length === 0 ? s.noFunctions : s.chooseFunction
    : args?.error ? s.argumentErrors[args.error]
    : valuesBlock ?? facts.descriptorError ?? facts.startBlock;
  // Dirty Native only saves and rereads; execution prerequisites are checked on the subsequent explicit Run.
  const prepare = state !== null && dirty && lane === 'native';
  const reason = (prepare && stale ? null : block) ?? (prepare ? null : setupBlock);
  const status = `${t.status.savedPackage} · ${validation === 'running' ? t.authoring.validating : t.status[validation]}`;
  const choice = state?.profileChoice ?? '';
  const profile = choice === '' ? s.chooseProfile : choice === 'values' ? s.explicitValues
    : choice.startsWith('profile:') ? bound?.profiles.find(item => item.id === choice.slice(8))?.name ?? choice.slice(8)
    : `${t.run.preset} · ${choice.slice(7)}`;
  // Native names the saved application window, otherwise the package's declared one; it never falls back on its own.
  const declared = bound?.package.target ?? null;
  const saved = bound?.target.view?.compatible ? bound.target.view.record.binding : null;
  const application = saved?.configuration.window_title || declared?.window_title || declared?.id || null;
  const nativeUnavailable = nativeCapability === null ? nativeError ? t.run.nativeCapabilityError : app.nativeUnavailable
    : declared === null ? s.noTarget : null;
  const nativeReady = lane === 'native' && nativeCapability !== null && native !== null;
  const targetMissing = nativeReady && (native.binding === null || native.block === 'nativeTarget' || native.block === 'nativeTargetDirty');
  const environmentMissing = environment === null && (lane === 'replay' || nativeReady);
  const entry = state?.unit === 'package' ? 'package' : candidate ? functionKey(candidate) : '';
  const argumentsKey = state?.selected ?? '';
  const argumentsWanted = candidate !== undefined && (candidate.required_arguments > 0 || args?.error != null || (args?.values?.length ?? 0) > 0);
  const activity = open ? authoring(leave) : null;
  const confirmedIntent = confirmation?.request.native_intent;
  const confirmedInput = confirmation?.target.configuration.input;
  const optionsLocked = locked || loading || dirty || block !== null || setupBlock !== null;
  return <>
    <button id="trial-setup-open" type="button" aria-haspopup="dialog" aria-label={s.openRun} aria-describedby="package-validation-status"
      title={status} onClick={show}>
      <ValidationStatusIcon id="package-validation-status" label={t.status.savedPackage} state={validation}/>{s.openRun}
    </button>
    <Modal id="trial-setup-dialog" className="trial-dialog" open={open} onCancel={() => onOpenChange(false)} labelledBy="trial-setup-heading"
      initialFocus="#trial-setup-heading" returnFocus={() => document.getElementById('trial-setup-open')}>
      <div className="dialog-header">
        <div className="trial-dialog-title"><h2 id="trial-setup-heading" tabIndex={-1}>{s.heading}</h2>
          <InfoHint id="trial-help" label={`${t.recognition.previewHelpButton} · ${s.heading}`} hint={s.help}/></div>
        <button id="trial-setup-close" type="button" className="icon" aria-label={t.common.close} title={t.common.close} onClick={() => onOpenChange(false)}>×</button>
      </div>
      {strip}
      <div className="dialog-body">
        {confirmation && confirmedIntent ? <section id="trial-confirmation" aria-labelledby="trial-confirm-heading">
          <h3 id="trial-confirm-heading" tabIndex={-1}>{s.confirmHeading}</h3>
          <p id="trial-native-warning" className="inline-warning">{s.nativeWarning}</p>
          <dl className="run-identity">
            <dt>{s.confirmedScript}</dt><dd id="trial-confirm-script">{confirmation.request.package_id} · {confirmation.request.function
              ? `${confirmation.request.function.module} · ${confirmation.request.function.function}` : s.package}</dd>
            <dt>{t.authoring.revision}</dt><dd id="trial-confirm-revision"><code>{confirmation.source.revision}</code></dd>
            <dt>{t.run.nativeApplication}</dt><dd id="trial-confirm-application" className="mono">{confirmation.target.configuration.game.path}</dd>
            <dt>{t.target.windowTitle}</dt><dd id="trial-confirm-window">{confirmation.target.configuration.window_title}</dd>
            <dt>{t.target.route}</dt><dd id="trial-confirm-route">{confirmedInput?.route === 'system' ? t.target.system : t.target.processDirected}</dd>
            <dt>{t.target.focus}</dt><dd id="trial-confirm-focus">{confirmedInput?.focus === 'preserve' ? t.target.preserve : t.target.requireFocused}</dd>
            <dt>{t.target.pointerMode}</dt><dd>{confirmedInput?.pointer_mode === 'core_graphics' ? t.target.coreGraphics : confirmedInput?.pointer_mode === 'appkit_background' ? t.target.appkitBackground : t.common.none}</dd>
            <dt>{t.target.clickHold}</dt><dd>{confirmedInput?.click_hold_ms} ms</dd>
            <dt>{t.run.nativeBinding}</dt><dd><code>{confirmedIntent.target_binding_id}</code> · {t.target.revision(confirmedIntent.target_revision)}</dd>
            <dt>{t.run.nextRunProfile}</dt><dd>{confirmation.profileName || t.run.profileDraft} · {confirmation.request.profile_id}</dd>
            <dt>{t.common.environment}</dt><dd>{confirmation.environment.profile} · {confirmation.environment.model} · {confirmation.environment.language} · {confirmation.environment.provider} · {confirmation.environment.runtime_profile}</dd>
            <dt>{t.run.nativeLaunchApproval}</dt><dd id="trial-confirm-launch">{confirmedIntent.launch_approved ? t.run.nativeLaunchApproved : t.run.nativeLaunchNotApproved}</dd>
            <dt>{t.run.nativeRecoveryApproval}</dt><dd id="trial-confirm-recovery">{confirmedIntent.max_exit_recoveries}</dd>
            <dt>{t.run.capturedBudgets}</dt><dd>{confirmation.request.function ? t.run.nativePhases.host_attachment : t.run.nativeStartup} {confirmedIntent.limits.startup_ms} ms · {confirmation.request.function ? t.run.nativePhases.instantiation : t.run.nativeReadiness} {confirmedIntent.limits.readiness_ms} ms · {confirmation.request.function ? s.function : t.run.nativeWorkflow} {confirmedIntent.limits.workflow_ms} ms</dd>
            <dt>{t.run.nativeDuration}</dt><dd>{t.run.nativeEnvelope((1 + confirmedIntent.max_exit_recoveries)
              * (confirmedIntent.limits.startup_ms + confirmedIntent.limits.readiness_ms + confirmedIntent.limits.workflow_ms)
              + confirmedIntent.max_exit_recoveries * 3000)}</dd>
          </dl>
          {confirmation.request.function && <><p className="field-help">{s.attachOnly}</p>
            <details><summary>{s.arguments}</summary><pre>{JSON.stringify(confirmation.request.function.arguments, null, 2)}</pre></details></>}
          <p className="field-help">{s.confirmHelp}</p>
          <p className="field-help">{t.run.nativeApprovalHelp}</p>
          {confirmedIntent.launch_approved && <div id="trial-confirm-recipe">
            <p className="field-help">{t.run.nativeLaunchHelp}</p>
            <dl className="run-identity">
              <dt>{t.run.nativeRecipient}</dt><dd className="mono">{(confirmation.target.configuration.launcher ?? confirmation.target.configuration.game).path}</dd>
              <dt>{t.run.nativeArguments}</dt><dd><code>{JSON.stringify(confirmation.target.configuration.arguments)}</code></dd>
              <dt>{t.run.nativeDirectory}</dt><dd>
                {confirmation.target.configuration.working_directory && <><code>{confirmation.target.configuration.working_directory}</code> · </>}
                {t.run.nativeDirectories[(confirmation.target.configuration.launcher ?? confirmation.target.configuration.game).kind === 'bundle'
                  ? confirmation.target.configuration.working_directory === null ? 'os' : 'refused'
                  : confirmation.target.configuration.working_directory === null ? 'parent' : 'explicit']}
              </dd>
            </dl>
            {confirmation.target.configuration.launcher && <p className="field-help">{t.run.nativeForwarding}</p>}
          </div>}
          {confirmedIntent.max_exit_recoveries > 0 && <p className="field-help">{t.run.nativeRecoveryHelp}</p>}
          <details><summary>{t.run.capturedOptions}</summary><pre>{JSON.stringify(confirmation.request.values, null, 2)}</pre></details>
          <details><summary>{t.run.capturedLimits}</summary><pre>{JSON.stringify(confirmedIntent.limits, null, 2)}</pre></details>
          <details><summary>{t.run.capturedEnvironment}</summary><pre>{JSON.stringify(confirmation.environment, null, 2)}</pre></details>
        </section> : <>
        {state && bound && facts && <>
          <div className="field"><label htmlFor="trial-entry">{s.unit}</label>
            <Select id="trial-entry" value={entry} disabled={locked} aria-describedby={candidate ? 'trial-signature' : undefined}
              onChange={value => handlers.change(current => value === 'package' ? {...current, unit:'package'} : {...current, unit:'function', selected:value})}
              options={[...(entry === '' ? [{value:'', label:s.chooseFunction, disabled:true}] : []), {value:'package', label:s.package},
                ...state.context.functions.map(item => {
                  const label = `${item.module} · ${item.function} · ${item.line}:${item.column}`;
                  return {value:functionKey(item), label, title:label, description:item.signature};
                })]}/>
            {candidate && <p id="trial-signature" className="trial-signature mono">{candidate.module}:{candidate.line}:{candidate.column} · {candidate.signature}</p>}
          </div>
          {state.unit === 'function' && <details id="trial-arguments-details" className="trial-arguments" open={argumentsOpen === argumentsKey || argumentsWanted}
            onToggle={event => setArgumentsOpen(event.currentTarget.open ? argumentsKey : null)}>
            <summary>{s.arguments} <code>{state.arguments}</code></summary>
            <div className="field">
              <textarea id="trial-arguments" value={state.arguments} disabled={locked} spellCheck={false} aria-label={s.arguments}
                aria-describedby="trial-arguments-help" aria-invalid={args?.error != null}
                onChange={event => handlers.change(current => ({...current, arguments:event.target.value}))}/>
              <p id="trial-arguments-help" className={args?.error ? 'field-error' : 'field-help'}>{args?.error ? s.argumentErrors[args.error] : s.argumentsHelp}</p>
            </div>
          </details>}
          <div className="field"><label htmlFor="trial-destination">{s.destination}</label>
            <Select id="trial-destination" value={bound.lane} disabled={locked}
              aria-describedby={`trial-destination-help${nativeUnavailable ? ' trial-native-unavailable' : ''}`}
              onChange={next => handlers.bound(item => chooseLane(item, next))}
              options={[
                {value:'native', label:application ? `${s.destinations.native} · ${application}` : s.destinations.native,
                  description:nativeUnavailable ?? s.destinationHelp.native, disabled:nativeUnavailable !== null},
                {value:'controlled', label:s.destinations.controlled, description:s.destinationHelp.controlled},
                {value:'replay', label:s.destinations.replay, description:s.destinationHelp.replay},
              ]}/>
            <p id="trial-destination-help" className="field-help">{s.destinationHelp[bound.lane === 'native' || bound.lane === 'replay' ? bound.lane : 'controlled']}</p>
            {nativeUnavailable && <p id="trial-native-unavailable" className="field-help">{s.destinations.native} · {nativeUnavailable}</p>}
          </div>
          {bound.lane === 'native' && nativeError && <FaultMessage title={t.run.nativeCapabilityError} value={nativeError}/>}
          {bound.lane === 'replay' && <div className="field"><label htmlFor="trial-replay-descriptor">{t.run.descriptor}</label>
            <input id="trial-replay-descriptor" type="text" value={bound.descriptorPath} disabled={locked} spellCheck={false}
              aria-invalid={facts.descriptorError !== null} placeholder={t.run.descriptorPlaceholder}
              onChange={event => handlers.bound(item => ({...item, descriptorPath:event.target.value}))}/>
            {facts.descriptorError && <p className="field-error">{facts.descriptorError}</p>}
          </div>}
          <dl id="trial-summary" className="run-identity">
            <dt>{t.run.nextRunProfile}</dt><dd>{profile}</dd>
          </dl>
          {valuesBlock && <div id="trial-values-block" className="inline-warning trial-prerequisite"><span>{valuesBlock}</span>
            <button id="trial-values-action" type="button" onClick={() => leave(() => handlers.setup('values'))}>{s.setupValues}</button></div>}
          {targetMissing && <div id="trial-target-block" className="inline-warning trial-prerequisite">
            <span>{native.block === 'nativeTarget' || native.block === 'nativeTargetDirty' ? app[native.block] : app.nativeBinding}</span>
            <button id="trial-target-action" type="button" onClick={() => leave(() => handlers.setup('target'))}>{s.setupTarget}</button></div>}
          {environmentMissing && <div id="trial-environment-block" className="inline-warning trial-prerequisite">
            <span>{bound.lane === 'replay' ? app.replayEnvironment : app.nativeEnvironment}</span>
            <button id="trial-environment-action" type="button" onClick={() => leave(handlers.environment)}>{s.setupEnvironment}</button></div>}
          {nativeReady && <>
            {state.unit === 'function' && <p className="field-help">{s.attachOnly}</p>}
            <details id="trial-native-options" className="trial-native-options" open={native.workflowError || undefined}>
              <summary>{s.nativeOptions}
                {native.launch && state.unit === 'package' && <span className="tag">{t.run.nativeLaunchApproval} · {t.run.nativeLaunchApproved}</span>}
                {native.recovery && state.unit === 'package' && <span className="tag">{t.run.nativeRecoveryApproval} · 1</span>}
              </summary>
              <div className="field"><label htmlFor="trial-workflow-seconds">{t.run.nativeWorkflowSeconds}</label>
                <input id="trial-workflow-seconds" type="text" inputMode="numeric" value={native.workflowSeconds} disabled={locked || loading}
                  aria-invalid={native.workflowError} aria-describedby="trial-workflow-help" spellCheck={false}
                  onChange={event => handlers.native.edit(event.target.value)}/>
                <p id="trial-workflow-help" className={native.workflowError ? 'field-error' : 'field-help'}>{native.workflowError
                  ? t.run.nativeWorkflowError(nativeCapability.max_workflow_ms / 1000)
                  : s.durationHelp}</p>
              </div>
              {native.limits && <details><summary>{t.run.nativeLimits}</summary><pre>{JSON.stringify(native.limits, null, 2)}</pre></details>}
              {state.unit === 'package' && <>
                <p className="field-help">{s.packageStartup}</p>
                {native.recipe && <dl className="run-identity" id="trial-native-recipe">
                  <dt>{t.run.nativeRecipient}</dt><dd>{native.recipe.recipient === 'game' ? t.run.nativeRecipientGame : t.run.nativeRecipientLauncher} · <code>{native.recipe.location.path}</code></dd>
                  <dt>{t.run.nativeArguments}</dt><dd><code>{JSON.stringify(native.recipe.arguments)}</code></dd>
                  <dt>{t.run.nativeDirectory}</dt><dd className={native.recipe.directory === 'refused' ? 'field-error' : undefined}>
                    {native.recipe.workingDirectory && <><code>{native.recipe.workingDirectory}</code> · </>}{t.run.nativeDirectories[native.recipe.directory]}</dd>
                </dl>}
                {native.recipe?.recipient === 'launcher' && <p className="field-help">{t.run.nativeForwarding}</p>}
                <label className="checkbox-label"><input id="trial-launch-consent" type="checkbox" checked={native.launch} disabled={optionsLocked}
                  onChange={event => handlers.native.approve('launch', event.target.checked)}/>{t.run.nativeLaunch}</label>
                <p className="field-help">{t.run.nativeLaunchHelp}</p>
                <label className="checkbox-label"><input id="trial-recovery-consent" type="checkbox" checked={native.recovery} disabled={optionsLocked || !native.launch}
                  onChange={event => handlers.native.approve('recovery', event.target.checked)}/>{t.run.nativeRecovery}</label>
                <p className="field-help">{t.run.nativeRecoveryHelp}</p>
              </>}
            </details>
          </>}
        </>}
        <div className="button-row trial-links">
          <button id="trial-advanced" type="button" className="link" onClick={() => leave(() => handlers.setup())}>{s.advancedSetup}</button>
          {hasResult && <>
            <button id="trial-results" type="button" className="link" onClick={() => leave(handlers.results)}>{s.openResults}</button>
            <button id="trial-logs" type="button" className="link" onClick={() => leave(handlers.logs)}>{s.logs}</button>
          </>}
          {state && !(stale && !dirty) && <button id="trial-reload" type="button" disabled={locked || loadBlocked} onClick={handlers.reload}>{s.refresh}</button>}
        </div>
        {state && dirty && <p className="field-help">{s.saveHelp}{lane === 'native' && <> {s.nativeSaveHelp}</>}</p>}
        </>}
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
        {confirmation
          ? <><button id="trial-confirm-back" type="button" onClick={handlers.cancelConfirmation}>{s.confirmBack}</button>
            <button id="trial-confirm-run" type="button" className="primary" disabled={locked || loading}
              aria-describedby="trial-native-warning" onClick={() => handlers.confirm(confirmation)}>{s.allowRun}</button></>
          : !state
          ? <button id="trial-load" type="button" className="primary" disabled={locked || loadBlocked || loading} onClick={handlers.reload}>{s.load}</button>
          : stale && !dirty
          // A clean Native setup read before the latest save rereads it here; consent never carries across revisions.
          ? <button id="trial-review-current" type="button" className="primary" disabled={locked || loadBlocked || loading} onClick={handlers.reload}>{s.reviewCurrent}</button>
          : prepare
          // Saving and rereading stay inside the dialog; startTrial stops before admission and asks for fresh review.
          ? <button id="trial-save-review" type="button" className="primary" disabled={locked || reason !== null} title={reason ?? undefined}
            onClick={handlers.run}>{s.saveReview}</button>
          : <button id="trial-run" type="button" className="primary" disabled={locked || reason !== null} title={reason ?? undefined}
            onClick={handlers.run}>{hasResult ? dirty ? s.saveRerun : s.rerun : dirty ? s.saveRun : s.run}</button>}
      </div>
    </Modal>
  </>;
}

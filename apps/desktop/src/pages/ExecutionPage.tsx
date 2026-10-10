import ResultPanel, {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {faultSummary, nativeOutcome, text} from '../state.ts';
import type {CheckAssociation} from '../state.ts';
import type {ControllerView, Json, NativeIntent} from '../types.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

// Immutable facts captured when this frontend submitted the operation; the host view is authoritative.
export type RunSnapshot =
  | {kind: 'run'; run: string; lane: string; packageId: string; profileName: string; profileId: string; scenario: string; descriptorPath: string | null; values: Record<string, Json>; native: NativeIntent | null}
  | {kind: 'check'; run: string; association: CheckAssociation};

export interface RunView {view: ControllerView; live: boolean; olderRevision: number | null}

interface Props {
  label: string; revision: number; run: RunView; snapshot: RunSnapshot | null; starting: boolean;
  disclosed: boolean; onDisclose: (next: boolean) => void;
}

export default function ExecutionPage({label, revision, run, snapshot, starting, disclosed, onDisclose}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const view = run.view;
  const phase = starting ? 'preparing' : view.state;
  const check = view.operation === 'environment_check';
  const captured = snapshot?.run === view.run ? snapshot : null;
  const primary = view.error ?? (view.result?.primary ? fault(view.result.primary) : null);
  const lane = captured?.kind === 'run' ? captured.lane : text(view.result?.lane);
  const privatePrimary = !check && lane !== 'controlled';
  const outcome = nativeOutcome(view);
  return <>
    <div className="page-heading"><div><span className="eyebrow">{t.common.revision(label, revision)}</span><h1 id="run-heading">{t.common.execution}</h1></div></div>
    <section className="panel" aria-labelledby="run-heading">
      <div className="panel-heading"><span className="eyebrow">{t.run.immutable}</span>
        <span className={`phase phase-${phase}`}>{t.phase(phase)}</span></div>
      <div className="panel-body">
        {run.olderRevision !== null && <p className="inline-warning">{t.run.olderRevision(run.olderRevision, revision)}</p>}
        {primary && (privatePrimary || check
          ? <section className="fault" role="alert"><strong>{check ? t.run.checkError : t.run.runError} · {faultSummary(primary, !privatePrimary)}</strong>
              <p>{t.run.diagnosticHelp}</p></section>
          : <FaultMessage title={t.run.runError} value={primary}/>)}
        {outcome?.failure && <p className="fault" id="native-failure">{outcome.cause ? t.run.nativeCauses[outcome.cause] : t.run.nativeFailures[outcome.failure]}</p>}
        {outcome?.launch && <p className="inline-warning" id="native-launch-outcome">{t.run.nativeLaunchOutcomes[outcome.launch]}</p>}
        {view.run === null ? <p className="muted" role="status">{starting ? t.run.submitted : t.run.noOperations}</p> : <>
          <p className="authority-note">{t.run.authority}</p>
          <dl className="run-identity"><dt>{t.run.operationId}</dt><dd id="run-id">{view.run}</dd>
            <dt>{t.run.kind}</dt><dd id="operation-kind">{check ? t.run.checkKind : t.run.runKind(t.lane(lane ?? t.common.unknown))}</dd>
            {view.native_preparation && <><dt>{t.run.nativeAttempt}</dt><dd id="native-attempt">{view.native_preparation.attempt}</dd>
              <dt>{t.run.nativeTargetStatus}</dt><dd id="native-target-status">{t.run.nativeStatuses[view.native_preparation.status]}</dd>
              <dt>{t.run.nativeStage}</dt><dd id="native-stage">{t.run.nativePhases[view.native_preparation.phase]}</dd>
              <dt>{t.run.nativeLaunchRequest}</dt><dd id="native-launch">{t.run.launchDispositions[view.native_preparation.launch]}</dd></>}
            {captured?.kind === 'run' && <><dt>{t.run.capturedProfile}</dt><dd>{captured.profileName || t.run.untitled} · {captured.profileId}</dd><dt>{t.run.packageScenario}</dt><dd>{captured.packageId} / {captured.scenario}</dd>
              {captured.lane === 'replay' && <><dt>{t.common.descriptor}</dt><dd>{captured.descriptorPath}</dd></>}
              {captured.native && <><dt>{t.run.nativeOperation}</dt><dd>{captured.native.operation}</dd>
                <dt>{t.run.nativePostcondition}</dt><dd>{captured.native.visible_postcondition}</dd>
                <dt>{t.run.nativeBinding}</dt><dd><code>{captured.native.target_binding_id}</code> · {t.target.revision(captured.native.target_revision)}</dd>
                <dt>{t.run.nativeLaunchApproval}</dt><dd>{captured.native.launch_approved ? t.run.nativeLaunchApproved : t.run.nativeLaunchNotApproved}</dd>
                <dt>{t.run.nativeRecoveryApproval}</dt><dd id="captured-native-recoveries">{captured.native.max_exit_recoveries}</dd>
                <dt>{t.run.capturedBudgets}</dt><dd id="captured-native-budgets">{t.run.nativeStartup} {captured.native.limits.startup_ms} ms · {t.run.nativeReadiness} {captured.native.limits.readiness_ms} ms · {t.run.nativeWorkflow} {captured.native.limits.workflow_ms} ms</dd></>}</>}
            {captured?.kind === 'check' && <><dt>{t.run.checkedProfile}</dt><dd>{captured.association.environment?.profile ?? t.common.unconfigured}</dd>
              <dt>{t.common.descriptor}</dt><dd>{captured.association.descriptorPath ?? t.run.noInitialization}</dd></>}
          </dl>
          {captured?.kind === 'run' && <details><summary>{t.run.capturedOptions}</summary><pre>{JSON.stringify(captured.values, null, 2)}</pre></details>}
          {captured?.kind === 'run' && captured.native && <details><summary>{t.run.capturedLimits}</summary><pre>{JSON.stringify(captured.native.limits, null, 2)}</pre></details>}
          {captured?.kind === 'check' && <details><summary>{t.run.capturedEnvironment}</summary><pre>{JSON.stringify(captured.association.environment, null, 2)}</pre></details>}
          <h3>{t.run.milestones}</h3><ol className="progress-list">{view.progress.map((event, index) => <li key={`${view.run}-${index}`}>
            <details><summary>{String(event.event ?? t.run.milestone)}{typeof event.attempt === 'number' ? ` · ${t.result.attempt(event.attempt)}` : ''}{text(event.stage) ? ` · ${event.stage}` : ''}{typeof event.at_us === 'number' ? ` · ${(event.at_us / 1000).toFixed(1)} ms` : ''}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>
          </li>)}</ol>{view.progress.length === 0 && <p className="muted">{t.run.noMilestones}</p>}
          <ResultPanel view={view} disclosed={disclosed} onDisclose={onDisclose}/>
        </>}
      </div>
    </section>
  </>;
}

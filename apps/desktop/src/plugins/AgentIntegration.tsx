import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import {ACTION_ORDER, canReview, executableChanged, pluginBusy} from './plugin-management.ts';
import type {PluginAction, PluginManagementState, PluginResult, PluginView} from './plugin-management.ts';

// Stable identity of the System Agent integration plugin; it names its list entry and its detail page.
export const AGENT_INTEGRATION = 'agent-integration';
export const AGENT_DETAILS_CONTROL = `plugin-details-${AGENT_INTEGRATION}`;

function resultKind(result: PluginResult) {
  return result.error !== null ? 'rejected' : result.outcome?.status ?? 'unknown';
}

// Observed facts of one OMP target; a confirmation shows the captured observation it will act on.
function TargetFacts({view}: {view: PluginView}) {
  const p = messages[useLocale()].ui.plugins;
  const compatibility = view.state === 'missing' || view.state === 'unsupported' || view.state === 'incompatible' ? view.state : 'compatible';
  return <dl className="plugin-facts">
    <dt>{p.executable}</dt><dd className="mono">{view.target?.executable ?? p.notFound}</dd>
    <dt>{p.ompVersion}</dt><dd>{view.target ? `${view.target.version} · ` : ''}{p.minimum(view.minimum_omp_version)}</dd>
    <dt>{p.userRoot}</dt><dd className="mono">{view.target?.user_root ?? p.notEstablished}</dd>
    <dt>{p.installed}</dt><dd>{view.installed ? `${view.installed.version} · ${view.installed.managed ? p.managed : p.unmanaged}` : view.state === 'absent' ? p.notInstalled : p.notEstablished}</dd>
    {view.installed && <><dt>{p.source}</dt><dd className="mono">{view.installed.source}</dd></>}
    <dt>{p.included}</dt><dd>{view.included_version}</dd>
    <dt>{p.compatibility}</dt><dd>{p.compatibilityStates[compatibility]}</dd>
  </dl>;
}

function ResultNotice({result}: {result: PluginResult}) {
  const p = messages[useLocale()].ui.plugins;
  const kind = resultKind(result);
  const heading = p.result(kind, result.action);
  const issue = kind === 'rejected' ? fault(result.error) : result.outcome?.issue ?? null;
  const target = result.target && <dl className="plugin-facts">
    <dt>{p.executable}</dt><dd className="mono">{result.target.executable}</dd>
    <dt>{p.userRoot}</dt><dd className="mono">{result.target.user_root}</dd>
  </dl>;
  if (kind === 'verified') return <div id="plugin-result" className="receipt plugin-result" tabIndex={-1} role="status">
    <strong>{heading}</strong>{target}<p>{p.freshSession[result.action]}</p>
  </div>;
  return <div id="plugin-result" className="plugin-result" tabIndex={-1}>
    {issue ? <FaultMessage title={heading} value={issue}/> : <p className="inline-warning" role="status"><strong>{heading}</strong></p>}
    {target}
    <p className="field-help">{p.resultHelp[kind]}</p>
  </div>;
}

// Compact list entry: identity, the observed OMP state and the attributable pending or latest outcome.
// Paths, management actions and guidance belong to the detail page.
export function AgentIntegrationEntry({state, admitted, onDetails}: {state: PluginManagementState; admitted: boolean; onDetails: () => void}) {
  const p = messages[useLocale()].ui.plugins;
  const {view, applying, result} = state;
  const outcome = result && resultKind(result);
  return <li className="plugin-entry" aria-labelledby="plugin-entry-agent-heading">
    <div className="plugin-entry-text">
      <h3 id="plugin-entry-agent-heading">{p.agentHeading}</h3>
      <p className="muted">{p.agentSummary}</p>
      <p className="plugin-entry-target"><strong>OMP</strong><span className="tag">{p.userScope}</span>
        {admitted && (state.inspecting ? <span className="muted">{p.inspecting}</span>
          : view ? <span className={`tag plugin-state-${view.state}`}>{p.state(view.state)}</span>
          : <span className="muted">{state.inspectError !== null ? p.inspectFailed : p.entryNotRead}</span>)}
      </p>
      {applying ? <p className="plugin-entry-activity">{p.running[applying.action]}</p>
        : result && outcome && <p className={`plugin-entry-activity plugin-outcome-${outcome}`}>{p.latestOperation} {p.result(outcome, result.action)}</p>}
    </div>
    <button id={AGENT_DETAILS_CONTROL} type="button" aria-labelledby={`${AGENT_DETAILS_CONTROL} plugin-entry-agent-heading`}
      onClick={onDetails}>{p.details}</button>
  </li>;
}

interface DetailProps {
  state: PluginManagementState; admitted: boolean;
  onEdit: (executable: string) => void; onRefresh: () => void; onCheck: () => void;
  onReview: (action: PluginAction) => void; onCancelReview: () => void; onConfirm: () => void;
}

// Detail page of the Agent integration: the OMP target observation, the reviewed installation actions with their
// read-back result, and the loaded-session guidance.
export function AgentIntegrationDetail({state, admitted, onEdit, onRefresh, onCheck, onReview, onCancelReview, onConfirm}: DetailProps) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const p = ui.plugins;
  const busy = pluginBusy(state);
  const {view, confirmation, applying} = state;
  const offered = view ? ACTION_ORDER.filter(action => view.actions.includes(action)) : [];
  return <section className="plugin-detail" aria-labelledby="plugin-agent-heading">
    <h3 id="plugin-agent-heading" tabIndex={-1}>{p.agentHeading}</h3>
    <p className="muted">{p.agentHelp}</p>
    <dl className="plugin-facts">
      <dt>{p.origin}</dt><dd>{p.originSystem}</dd>
      <dt>{p.activation}</dt><dd>{p.activationInstallation}</dd>
    </dl>
    <h4 id="plugin-targets-heading">{p.targets}</h4>
    <ul className="plugin-targets" aria-labelledby="plugin-targets-heading">
      <li className="plugin-target" aria-labelledby="plugin-omp-heading">
        <div className="plugin-target-heading">
          <strong id="plugin-omp-heading">OMP</strong>
          <span className="tag">{p.userScope}</span>
          {admitted && view && <span className={`tag plugin-state-${view.state}`}>{p.state(view.state)}</span>}
          <button id="plugins-refresh" type="button" disabled={!admitted || busy} onClick={onRefresh}>{p.refresh}</button>
        </div>
        <div className="field plugin-executable">
          <label htmlFor="plugin-executable">{p.executableLabel}</label>
          <div className="plugin-executable-row">
            <input id="plugin-executable" type="text" value={state.executable} spellCheck={false} autoComplete="off" autoCapitalize="off"
              placeholder={p.executablePlaceholder} aria-describedby="plugin-executable-help" onChange={event => onEdit(event.target.value)}/>
            <button id="plugin-check" type="button" disabled={!admitted || busy} onClick={onCheck}>{p.check}</button>
          </div>
          <p id="plugin-executable-help" className="field-help">{p.executableHelp}</p>
        </div>
        {admitted && <>
          {state.inspectError !== null && <><FaultMessage title={p.inspectFailed} value={fault(state.inspectError)}/>
            <p className="field-help">{p.inspectFailedHelp}</p></>}
          {view === null && state.inspectError === null && <p className="muted">{state.inspecting ? p.inspecting : p.notRead}</p>}
          {view && <>
            <TargetFacts view={view}/>
            <details className="plugin-identity"><summary>{p.identity}</summary>
              <dl className="plugin-facts"><dt>{p.includedContent}</dt><dd className="mono">{view.included_content}</dd>
                <dt>{p.installedContent}</dt><dd className="mono">{view.installed ? view.installed.content ?? p.unknownContent : view.state === 'absent' ? p.notInstalled : p.notEstablished}</dd></dl>
            </details>
            <p className="plugin-state-help">{p.stateHelp(view.state)}</p>
            {view.issue && <FaultMessage title={p.issue} value={view.issue}/>}
            {executableChanged(state) && <p className="inline-warning">{p.executableChanged}</p>}
            {state.stale && <p className="inline-warning">{p.stale}</p>}
            {state.withdrawn && <p className="inline-warning" role="status">{p.withdrawn}</p>}
            {offered.length > 0
              ? <div className="button-row">{offered.map(action => <button key={action} id={`plugin-action-${action}`} type="button"
                className={action === 'uninstall' ? undefined : 'primary'} disabled={!canReview(state, action)}
                aria-expanded={confirmation?.action === action} aria-controls={confirmation?.action === action ? 'plugin-confirmation' : undefined}
                onClick={() => onReview(action)}>{p.actions[action]}</button>)}</div>
              : !applying && <p className="muted">{p.noActions}</p>}
            {confirmation && <section id="plugin-confirmation" className="plugin-confirmation" aria-labelledby="plugin-confirm-heading">
              <h5 id="plugin-confirm-heading">{p.confirmHeading[confirmation.action]}</h5>
              <p>{p.confirmBody(confirmation.action, {included: confirmation.view.included_version,
                installed: confirmation.view.installed?.version ?? p.notInstalled, source: confirmation.view.installed?.source ?? p.notInstalled})}</p>
              <TargetFacts view={confirmation.view}/>
              {confirmation.action !== 'install' && <p>{p.sessionDisconnect} <code>{'/mado disconnect {}'}</code></p>}
              <p className="field-help">{p.confirmScope}</p>
              <div className="button-row">
                <button id="plugin-confirm-cancel" type="button" onClick={onCancelReview}>{ui.common.cancel}</button>
                <button id="plugin-confirm" type="button" className={confirmation.action === 'uninstall' ? 'danger-text' : 'primary'}
                  onClick={onConfirm}>{p.confirm[confirmation.action]}</button>
              </div>
            </section>}
          </>}
        </>}
        {applying && <div id="plugin-pending" className="inline-info plugin-pending" tabIndex={-1}>
          <strong>{p.running[applying.action]}</strong>
          <p>{p.pending(applying.view.target?.executable ?? p.notFound)}</p>
        </div>}
        {!applying && state.result && <ResultNotice result={state.result}/>}
      </li>
    </ul>
    <section className="plugin-sessions" aria-labelledby="plugin-sessions-heading">
      <h4 id="plugin-sessions-heading">{p.sessionHeading}</h4>
      <p>{p.sessionHelp}</p>
      <p>{p.sessionDisconnect} <code>{'/mado disconnect {}'}</code></p>
      <p>{p.sessionFresh} <code>/extensions</code></p>
    </section>
  </section>;
}

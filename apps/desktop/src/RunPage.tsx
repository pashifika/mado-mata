import {useState} from 'react';
import SchemaForm from './SchemaForm.tsx';
import ResultPanel, {FaultMessage, fault} from './ResultPanel.tsx';
import {faultSummary, text} from './state.ts';
import type {CheckAssociation} from './state.ts';
import {DESCRIPTOR_LIMIT, busy, editDraft} from './workspace.ts';
import type {Workspace} from './workspace.ts';
import type {ControllerView, Json, OcrEnvironment, Profile} from './types.ts';

// Immutable facts captured when this frontend submitted the operation; the host view is authoritative.
export type RunSnapshot =
  | {kind: 'run'; run: string; lane: string; packageId: string; profileName: string; profileId: string; scenario: string; descriptorPath: string | null; values: Record<string, Json>}
  | {kind: 'check'; run: string; association: CheckAssociation};

export interface RunView {view: ControllerView; live: boolean; olderRevision: number | null}

export interface Derived {
  parsed: {values: Record<string, Json>; errors: Record<string, string>};
  numericErrors: boolean; valuesDirty: boolean; dirty: boolean; bound: boolean;
  selectedProfile: Profile | undefined; startBlock: string | null; descriptorError: string | null;
}

export interface RunHandlers {
  change: (update: (workspace: Workspace) => Workspace) => void;
  validate: () => void; saveProfile: () => void; renameProfile: () => void; deleteProfile: () => void;
  newDraft: (preset?: string) => void; selectProfile: (id: string) => void;
  reinspect: () => void; start: () => void; stop: () => void;
}

interface Props {
  workspace: Workspace; label: string; derived: Derived; run: RunView; snapshot: RunSnapshot | null;
  locked: boolean; active: boolean; starting: boolean; stopping: boolean; closing: boolean;
  savedEnvironment: OcrEnvironment | null; handlers: RunHandlers;
}

const LANE_LABEL: Record<string, string> = {controlled: 'Controlled', replay: 'Recorded replay', native: 'Native · refused'};

export default function RunPage({workspace, label, derived, run, snapshot, locked, active, starting, stopping, closing, savedEnvironment, handlers}: Props) {
  const {parsed, numericErrors, valuesDirty, dirty, bound, selectedProfile, startBlock, descriptorError} = derived;
  const [confirmReinspect, setConfirmReinspect] = useState(false);
  const view = run.view;
  const phase = starting ? 'preparing' : view.state;
  const check = view.operation === 'environment_check';
  const primary = view.error ?? (view.result?.primary ? fault(view.result.primary) : null);
  const privatePrimary = !check && (snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane)) !== 'controlled';
  const selectedDescriptor = workspace.descriptorPath.trim() || null;
  const canStart = !locked && !active && !numericErrors && bound && startBlock === null && descriptorError === null;
  const stopAvailable = run.live && view.run !== null && busy(view.state) && view.state !== 'stopping' && !stopping && !starting && !closing;
  const executionCaption = starting ? 'Start submitted · awaiting admission' : run.live && busy(view.state) ? `Operation ${view.run} owns the runner` : view.state === 'terminal' ? `Operation ${view.run} settled` : 'No operation for this workspace yet';
  const heading = selectedProfile && !valuesDirty ? selectedProfile.name : workspace.name || 'Untitled draft';
  return <>
    <div className="page-heading"><div><span className="eyebrow">{label} / Run control</span><h1>{heading}</h1>
      <p>Edits affect the next operation, never the active snapshot. Every Start revalidates files; no check or inspection is reused.</p></div>
      <div className="actions">
        <button id="validate" disabled={locked || numericErrors || !bound} onClick={handlers.validate}>Validate</button>
        <button id="start" className="primary" disabled={!canStart} onClick={handlers.start}>Start</button>
      </div>
    </div>
    <div id="error">
      {workspace.error && <FaultMessage title="Action failed" value={workspace.error}/>}
      {primary && (privatePrimary || check
        ? <section className="fault" role="alert"><strong>{check ? 'Primary check error' : 'Primary run error'} · {faultSummary(primary, !privatePrimary)}</strong>
            <p>Full diagnostics are available through explicit private disclosure in the Execution panel.</p></section>
        : <FaultMessage title="Primary run error" value={primary}/>)}
    </div>
    <div className="operation-status" role="status">{workspace.busy || workspace.notice || (startBlock ?? '')}</div>
    <section className="panel summary-panel" aria-label="Workspace summary">
      <div className="summary-item"><span className="eyebrow">Execution</span>
        <div className="state-line"><span className={`dot phase-${phase}`} aria-hidden="true"/><span id="state" className={`phase phase-${phase}`}>{phase}</span></div>
        <p className="summary-caption">{executionCaption}</p></div>
      <div className="summary-item"><span className="eyebrow">Profile</span>
        <div className="summary-value">{selectedProfile ? selectedProfile.name : workspace.name || 'Untitled draft'}</div>
        <p className={`summary-caption save-status ${dirty ? 'unsaved' : ''}`}>{selectedProfile ? dirty ? 'Saved profile · draft has changes' : 'Saved · no changes' : 'Draft · not saved'}</p></div>
      <div className="summary-item"><span className="eyebrow">Input route</span>
        <div className="summary-value">{LANE_LABEL[workspace.lane] ?? workspace.lane}</div>
        <p className="summary-caption">{workspace.lane === 'replay' ? 'Recorded corpus, real recognition, controlled sink' : 'Fixture observations, no live capture or input'}</p></div>
    </section>
    <div className="run-grid">
      <section className="panel" aria-labelledby="config-heading">
        <div className="panel-heading"><h2 id="config-heading">Run configuration</h2><span className="tag">{dirty ? 'Unsaved changes' : 'Saved values'}</span></div>
        <div className="panel-body">
          <div className="package-card">
            <div className="package-text"><strong>{workspace.package.package_id}</strong>
              <span className="mono" title={workspace.packagePath}>{workspace.package.runtime} · {workspace.packagePath}</span></div>
            <span className="tag">Inspected · revision {workspace.revision}</span>
            {confirmReinspect
              ? <div className="confirm-row" role="group" aria-label="Confirm reinspect">
                <span>Discard the unsaved draft and reinspect this package?</span>
                <button type="button" className="danger-text" onClick={() => {setConfirmReinspect(false); handlers.reinspect();}}>Discard and reinspect</button>
                <button type="button" onClick={() => setConfirmReinspect(false)}>Keep draft</button></div>
              : <button id="reinspect" disabled={locked || starting || (run.live && busy(view.state))} title={run.live && busy(view.state) ? 'Inspection is refused while this workspace owns an operation' : undefined}
                onClick={() => dirty && workspace.touched ? setConfirmReinspect(true) : handlers.reinspect()}>Reinspect</button>}
          </div>
          <p className="field-help">Reinspection validates inventory, schema, and static dependencies without executing package code; it increments the selection revision and resets the draft.</p>
          {workspace.profilesError && <><FaultMessage title="Some saved profiles could not be loaded" value={workspace.profilesError}/><p className="muted">Compatible profiles and new drafts remain available. Stored files were not modified. Restore compatible data, then reinspect.</p></>}
          <div className="two-col">
            <div className="field"><label htmlFor="profile-select">Saved profile</label>
              <select id="profile-select" value={workspace.selectedId ?? ''} disabled={locked} onChange={event => handlers.selectProfile(event.target.value)}>
                <option value="">Unsaved draft</option>{workspace.profiles.map(profile => <option key={profile.id} value={profile.id}>{profile.name}</option>)}
              </select>
              {selectedProfile && <p className="identity-text">{selectedProfile.id}</p>}
              {!bound && <p className="inline-warning">Stale schema identity. This profile cannot be rebound or run.</p>}</div>
            <div className="field"><label htmlFor="preset-select">Package preset</label>
              <select id="preset-select" value={workspace.preset} disabled={locked} onChange={event => handlers.newDraft(event.target.value)}>
                <option value="">Top-level defaults</option>{Object.keys(workspace.package.profiles).map(key => <option key={key} value={key}>{key}</option>)}
              </select><p className="field-help">Choosing a preset creates a new draft. Save it with a name to keep it.</p></div>
          </div>
          <div className="field"><label htmlFor="profile-name">Profile name</label>
            <input id="profile-name" value={workspace.name} disabled={locked} onChange={event => handlers.change(item => ({...item, name: event.target.value, notice: '', touched: true}))}/></div>
          <div className="button-row">
            <button id="save-profile" className="primary" disabled={locked || !workspace.name.trim() || numericErrors || !bound} onClick={handlers.saveProfile}>{workspace.selectedId ? 'Update profile' : 'Save new profile'}</button>
            <button id="new-profile" disabled={locked} onClick={() => handlers.newDraft()}>New draft</button>
            <button id="rename-profile" disabled={!workspace.selectedId || locked || !workspace.name.trim() || !bound} onClick={handlers.renameProfile}>Rename</button>
            <button id="delete-profile" className="danger-text" disabled={!workspace.selectedId || locked} onClick={handlers.deleteProfile}>Delete profile</button>
          </div>
          <h3>Options</h3>
          <p className="field-help">Missing top-level fields use schema defaults. Explicit objects are never recursively filled. Backend validation reports exact field paths.</p>
          <SchemaForm schema={workspace.package.schema} value={workspace.draft} onChange={draft => handlers.change(item => editDraft(item, draft))} errors={parsed.errors}/>
          {workspace.validation && <details className="validated"><summary>Valid · effective values</summary><pre>{JSON.stringify(workspace.validation, null, 2)}</pre></details>}
          <h3>Execution route</h3>
          <div className="two-col">
            <div className="field"><label htmlFor="lane">Execution lane</label>
              <select id="lane" value={workspace.lane} disabled={locked} onChange={event => handlers.change(item => ({...item, lane: event.target.value, notice: ''}))}>
                <option value="controlled">Controlled · fixture observations, no recognition</option>
                <option value="replay">Replay · recorded corpus, real recognition, controlled sink</option>
                <option value="native" disabled>Native · refused, no live capture or input</option>
              </select></div>
            <div className="field"><label htmlFor="scenario">Controlled scenario</label>
              <select id="scenario" value={workspace.lane === 'replay' ? 'workflow' : workspace.scenario} disabled={locked || workspace.lane === 'replay'} onChange={event => handlers.change(item => ({...item, scenario: event.target.value}))}>
                <option value="workflow">Workflow</option><option value="held-work">Held work · exercise Stop</option><option value="no-match">No match</option>
              </select></div>
          </div>
          <div className="field"><label htmlFor="descriptor-path">Recorded corpus descriptor</label>
            <input id="descriptor-path" type="text" value={workspace.descriptorPath} disabled={locked} spellCheck={false} aria-invalid={descriptorError !== null}
              placeholder="Absolute path to a replay descriptor JSON" onChange={event => handlers.change(item => ({...item, descriptorPath: event.target.value, notice: ''}))}/>
            {descriptorError && <p className="field-error">{descriptorError}</p>}
            <p className="field-help">A location hint for this workspace session only, bounded to {DESCRIPTOR_LIMIT} bytes. Replay Start and OCR Check read the descriptor and its declared assets through this package's inventory each time; unknown or native fields are refused.</p></div>
          <p className="muted">Start uses {selectedProfile && !valuesDirty ? `saved profile “${selectedProfile.name}”` : 'the explicit draft shown here'}{workspace.lane === 'replay' ? `, the saved OCR environment${savedEnvironment ? ` (${savedEnvironment.profile})` : ' (none saved)'}, and descriptor ${selectedDescriptor ?? '(none)'}` : ''}.</p>
          {startBlock && <p className="inline-warning" id="start-block">{startBlock}</p>}
        </div>
      </section>
      <section className="panel" aria-labelledby="run-heading">
        <div className="panel-heading"><div><span className="eyebrow">Immutable operation</span><h2 id="run-heading">{check ? 'Environment check' : 'Execution'}</h2></div>
          <span className={`phase phase-${phase}`}>{phase}</span></div>
        <div className="panel-body">
          {run.olderRevision !== null && <p className="inline-warning">This outcome belongs to selection revision {run.olderRevision}; the workspace is now at revision {workspace.revision}. It is not evidence for the current draft.</p>}
          <div className="run-buttons">
            <button id="stop" className="stop-button" disabled={!stopAvailable} onClick={handlers.stop}>{stopping ? 'Requesting Stop…' : 'Stop'}</button>
            <span className="muted">Stop targets operation <code>{run.live && view.run ? view.run : 'none'}</code> regardless of the visible workspace.</span>
          </div>
          <p className="authority-note">Submitted does not prove effect; Stop does not prove cleanup. Stop covers checks and runs alike.</p>
          <dl className="run-identity"><dt>Operation ID</dt><dd id="run-id">{view.run ?? 'No operation yet'}</dd>
            <dt>Kind</dt><dd id="operation-kind">{view.run ? check ? 'Environment check · no package code' : `Run · ${snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane) ?? 'unknown'} lane` : 'Idle'}</dd>
            {snapshot?.kind === 'run' && snapshot.run === view.run && <><dt>Captured profile</dt><dd>{snapshot.profileName} · {snapshot.profileId}</dd><dt>Package / scenario</dt><dd>{snapshot.packageId} / {snapshot.scenario}</dd>
              {snapshot.lane === 'replay' && <><dt>Descriptor</dt><dd>{snapshot.descriptorPath}</dd></>}</>}
            {snapshot?.kind === 'check' && snapshot.run === view.run && <><dt>Checked profile</dt><dd>{snapshot.association.environment?.profile ?? 'unconfigured'}</dd>
              <dt>Descriptor</dt><dd>{snapshot.association.descriptorPath ?? 'none · initialization not attempted'}</dd></>}
          </dl>
          {snapshot?.kind === 'run' && snapshot.run === view.run && <details><summary>Captured options · unchanged by draft edits</summary><pre>{JSON.stringify(snapshot.values, null, 2)}</pre></details>}
          {snapshot?.kind === 'check' && snapshot.run === view.run && <details><summary>Captured saved environment · unchanged by later edits</summary><pre>{JSON.stringify(snapshot.association.environment, null, 2)}</pre></details>}
          <h3>Progress milestones</h3><ol className="progress-list">{view.progress.map((event, index) => <li key={`${view.run}-${index}`}>
            <details><summary>{String(event.event ?? 'Milestone')}{text(event.stage) ? ` · ${event.stage}` : ''}{typeof event.at_us === 'number' ? ` · ${(event.at_us / 1000).toFixed(1)} ms` : ''}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>
          </li>)}</ol>{view.progress.length === 0 && <p className="muted">No milestones recorded.</p>}
          <ResultPanel view={view} disclosed={view.run !== null && workspace.disclosedRun === view.run} onDisclose={next => handlers.change(item => ({...item, disclosedRun: next ? view.run : null}))}/>
        </div>
      </section>
    </div>
  </>;
}

import {useEffect, useMemo, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {getCurrentWindow} from '@tauri-apps/api/window';
import SchemaForm from './SchemaForm.tsx';
import EnvironmentPanel from './EnvironmentPanel.tsx';
import type {LastCheck} from './EnvironmentPanel.tsx';
import {DISCLOSURE_LIMIT, acceptController, boundedText, cleanupLabel, defaultDraft, environmentDraft, faultSummary, initializationLabel, readDraft, readEnvironment, record, retainLogs, sameEnvironment, staleReasons, text} from './state.ts';
import type {CheckAssociation, EnvironmentDraft, LogStore} from './state.ts';
import type {ControllerView, Fault, Json, LogBatch, PackageInfo, Poll, Profile, Selection, Settings} from './types.ts';

const idle: ControllerView = {run: null, state: 'idle', operation: 'run', result: null, error: null, progress: [], dropped_logs: 0};
const busyPhases = new Set(['preparing', 'running', 'stopping']);
type Losses = Omit<LogBatch, 'entries'>;
type RunSnapshot =
  | {kind: 'run'; run: string; lane: string; packageId: string; profileName: string; profileId: string; scenario: string; descriptorPath: string | null; values: Record<string, Json>}
  | {kind: 'check'; run: string; association: CheckAssociation};

function fault(error: unknown): Fault {
  if (error !== null && typeof error === 'object' && 'message' in error) {
    const value = error as Partial<Fault>;
    return {category: value.category ?? 'Application', message: String(value.message), context: value.context ?? null};
  }
  return {category: 'Application', message: String(error), context: null};
}

function FaultMessage({value, title}: {value: Fault; title: string}) {
  return <section className="fault" role="alert"><strong>{title} · {value.category}</strong><p>{value.message}</p>
    {value.context !== null && <pre className="diagnostic">{JSON.stringify(value.context, null, 2)}</pre>}
  </section>;
}

function BoundedRecord({value}: {value: Json}) {
  const bounded = boundedText(JSON.stringify(value, null, 2), DISCLOSURE_LIMIT);
  return <>
    <pre>{bounded.text}</pre>
    {bounded.truncated > 0 && <p className="inline-warning">Display truncated: {bounded.truncated} more characters are retained by the backend record but not rendered.</p>}
  </>;
}

function ResultPanel({view, disclosed, onDisclose}: {view: ControllerView; disclosed: boolean; onDisclose: (next: boolean) => void}) {
  const result = view.result;
  // Preparation faults carry the same operation facts in their context as a settled result.
  const source = result ?? record(view.error?.context);
  const observations = record(result?.observations);
  const build = record(result?.build);
  const check = view.operation === 'environment_check';
  const lane = text(result?.lane) ?? text(source.lane);
  // Replay observations and script decisions can carry recognized text: private until disclosed.
  const privateDetail = !check && lane !== null && lane !== 'controlled';
  const completed = Array.isArray(source.completed_stages) ? source.completed_stages.map(String) : [];
  return <>
    <div className="result-facts">
      <div><span>Result status</span><strong>{String(result?.status ?? (view.state === 'terminal' && view.error ? view.error.category === 'Cancelled' ? 'Cancelled' : 'Refused' : 'Not settled'))}</strong></div>
      <div><span>Entry outcome</span><strong>{String(result?.entry_outcome ?? 'Unobserved')}</strong></div>
      <div><span>Cleanup</span><strong>{cleanupLabel(result, source)}</strong></div>
      <div><span>Forced containment</span><strong>{result?.forced === true ? 'Yes' : result?.forced === false ? 'No' : 'Unobserved'}</strong></div>
    </div>
    <dl className="run-identity identity-facts" id="identity-facts">
      <dt>Stage</dt><dd>{text(source.stage) ?? (view.state === 'idle' ? 'No operation yet' : 'Unobserved')}</dd>
      <dt>Completed</dt><dd>{completed.length ? completed.join(' → ') : 'None recorded'}</dd>
      <dt>Environment</dt><dd><code>{text(source.environment_identity) ?? 'Not derived'}</code></dd>
      <dt>Corpus</dt><dd><code>{text(source.corpus_identity) ?? 'Not derived'}</code></dd>
      {check && <><dt>Selection</dt><dd><code>{text(source.selection_identity) ?? 'Unobserved'}</code></dd><dt>Initialization</dt><dd id="initialization">{initializationLabel(view.progress)}</dd></>}
      <dt>Child build</dt><dd>{build.engine_enabled === undefined ? 'No startup identity observed' : <>engine {build.engine_enabled === true ? 'enabled' : 'absent'} · <code>{text(build.executable_sha256) ?? 'unhashed'}</code></>}</dd>
    </dl>
    {result && check && <div className="outcome-details">
      <p className="muted">No package module, readiness, or workflow code was evaluated. Engine facts are the child's own report.</p>
      <details><summary>Engine facts</summary><BoundedRecord value={observations.engine ?? null}/></details>
      <details><summary>Cleanup evidence</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    {result && !check && <div className="outcome-details">
      {privateDetail && !disclosed
        ? <p className="muted">Script decisions and observation records stay hidden until disclosed below. Full records are not automatically copied to ordinary logs; scripts can explicitly emit bounded messages.</p>
        : <>
          <h3>Script decisions</h3>
          {Array.isArray(observations.logs) && observations.logs.length > 0
            ? <ul className="decision-list">{observations.logs.map((item, index) => <li key={index}>{typeof item === 'string' ? item : JSON.stringify(item)}</li>)}</ul>
            : <p className="muted">No decision was recorded.</p>}
          <div className="evidence-grid">{['dispatches', 'receipts', 'accepted', 'effects', 'postconditions', 'sink', ...(privateDetail ? ['engine'] : [])].map(key => <details key={key}>
            <summary>{key}{Array.isArray(observations[key]) ? ` · ${observations[key].length}` : ''}</summary>
            <BoundedRecord value={observations[key] ?? null}/>
          </details>)}</div>
        </>}
      <details><summary>Cleanup evidence</summary><pre>{JSON.stringify(result.cleanup ?? null, null, 2)}</pre></details>
    </div>}
    <div id="result" className="private-disclosure">
      <button id="disclose-result" disabled={!result && !view.error} onClick={() => onDisclose(!disclosed)}>{disclosed ? 'Hide full record' : 'Disclose full record · private'}</button>
      <span className="muted">Build metadata, diagnostics{privateDetail ? ', and recognition detail' : ''}. Rendered here only, bounded to {DISCLOSURE_LIMIT / 1024} KiB.</span>
      {disclosed && <BoundedRecord value={result ?? (view.error ? {category: view.error.category, message: view.error.message, context: view.error.context} : null)}/>}
    </div>
  </>;
}

export default function App() {
  const [packagePath, setPackagePath] = useState('');
  const [selection, setSelection] = useState<PackageInfo | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [profilesError, setProfilesError] = useState<Fault | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [preset, setPreset] = useState('');
  const [draft, setDraft] = useState<Record<string, Json>>({});
  const [validation, setValidation] = useState<Record<string, Json> | null>(null);
  const [scenario, setScenario] = useState('workflow');
  const [lane, setLane] = useState('controlled');
  const [descriptorPath, setDescriptorPath] = useState('');
  const [envDraft, setEnvDraft] = useState<EnvironmentDraft>(environmentDraft(null));
  const [lastCheck, setLastCheck] = useState<LastCheck | null>(null);
  const [disclosed, setDisclosed] = useState(false);
  const [view, setView] = useState<ControllerView>(idle);
  const [snapshot, setSnapshot] = useState<RunSnapshot | null>(null);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [closing, setClosing] = useState(false);
  const [operation, setOperation] = useState('Loading settings');
  const [error, setError] = useState<Fault | null>(null);
  const [pollError, setPollError] = useState<Fault | null>(null);
  const [notice, setNotice] = useState('');
  const [settings, setSettings] = useState<Settings | null>(null);
  const [logLimit, setLogLimit] = useState('1000');
  const [logs, setLogs] = useState<LogStore>({items: [], evicted: 0});
  const [losses, setLosses] = useState<Losses>({gui_dropped: 0, file_dropped: 0, file_errors: 0, last_file_error: null});
  const expectedRun = useRef<string | null>(null);
  const epoch = useRef(0);
  const startInFlight = useRef(false);
  const commandInFlight = useRef(true);
  const revision = useRef(0);
  const retention = useRef(1000);
  const envRevision = useRef(0);
  const selectedProfile = profiles.find(profile => profile.id === selectedId);
  const parsed = useMemo(() => selection ? readDraft(selection.schema, draft) : {values: draft, errors: {}}, [selection, draft]);
  const numericErrors = Object.keys(parsed.errors).length > 0;
  const valuesDirty = !selectedProfile || numericErrors || JSON.stringify(parsed.values) !== JSON.stringify(selectedProfile.values);
  const dirty = valuesDirty || name !== selectedProfile?.name;
  const bound = !selectedProfile || (selectedProfile.package_id === selection?.package_id && selectedProfile.schema_identity === selection?.schema_identity);
  const active = starting || busyPhases.has(view.state);
  const locked = Boolean(operation) || starting || closing;
  const primary = view.error ?? (view.result?.primary ? fault(view.result.primary) : null);
  const privatePrimary = view.operation !== 'environment_check' &&
    (snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane)) !== 'controlled';
  const envParsed = useMemo(() => readEnvironment(envDraft), [envDraft]);
  const envErrors = Object.keys(envParsed.errors).length > 0;
  const savedEnvironment = settings?.ocr_environment ?? null;
  const envDirty = envErrors || !sameEnvironment(envParsed.environment, savedEnvironment);
  const selectedDescriptor = descriptorPath.trim() || null;
  const checkStale = lastCheck ? staleReasons(lastCheck.association, {
    saved: savedEnvironment, draftDirty: envDirty, descriptorPath: selectedDescriptor, packageInventoryIdentity: selection?.inventory_identity ?? null,
  }) : [];
  const startBlock = lane === 'replay' && !selectedDescriptor ? 'Replay needs a recorded corpus descriptor path.'
    : lane === 'replay' && !savedEnvironment ? 'Replay needs a saved OCR environment.' : null;

  // A terminal check is retained independently of later runs, keyed to what it observed.
  useEffect(() => {
    if (snapshot?.kind === 'check' && view.run === snapshot.run && view.state === 'terminal') {
      setLastCheck({association: snapshot.association, view});
    }
  }, [view, snapshot]);

  function editEnvironment(next: EnvironmentDraft) {
    envRevision.current += 1;
    setEnvDraft(next);
    setNotice('');
  }

  function editDraft(next: Record<string, Json>) {
    revision.current += 1;
    setDraft(next);
    setValidation(null);
    setNotice('');
  }

  function newDraft(info: PackageInfo, presetName = '') {
    setSelectedId(null);
    setName(presetName);
    setPreset(presetName);
    editDraft(presetName ? structuredClone(info.profiles[presetName].options) : defaultDraft(info.schema));
  }

  function forgetSelection() {
    setSelection(null);
    setProfiles([]);
    setProfilesError(null);
    setSelectedId(null);
    setName('');
    setPreset('');
    editDraft({});
  }

  async function inspectPath(path: string) {
    forgetSelection();
    setError(null);
    setNotice('');
    try {
      const selected = await invoke<Selection>('inspect', {packagePath: path});
      setSelection(selected.package);
      setProfiles(selected.profiles);
      setProfilesError(selected.profiles_error);
      setSettings(old => old ? {...old, package_path: path} : old);
      newDraft(selected.package);
      setNotice('Package inspected. Start will revalidate its identity and capture current values.');
    } catch (cause) {
      setError(fault(cause));
      setNotice('No package is authorized for Start. Correct the directory or package, then Inspect again.');
    }
  }

  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    async function tick() {
      const pollEpoch = epoch.current;
      try {
        const incoming = await invoke<Poll>('poll');
        if (!alive) return;
        // Drained logs belong to the application stream, even if the controller snapshot is stale.
        if (incoming.logs.entries.length) setLogs(old => retainLogs(old, incoming.logs.entries, retention.current));
        setLosses(old => old.gui_dropped === incoming.logs.gui_dropped && old.file_dropped === incoming.logs.file_dropped
          && old.file_errors === incoming.logs.file_errors && old.last_file_error === incoming.logs.last_file_error ? old : {
            gui_dropped: incoming.logs.gui_dropped, file_dropped: incoming.logs.file_dropped,
            file_errors: incoming.logs.file_errors, last_file_error: incoming.logs.last_file_error,
          });
        setPollError(null);
        if (pollEpoch === epoch.current && !startInFlight.current) {
          const next = {...idle, ...incoming.controller};
          setView(current => {
            if (pollEpoch !== epoch.current || startInFlight.current) return current;
            const accepted = acceptController(current, next, expectedRun.current);
            if (accepted === current) return current;
            if (current.run === accepted.run && current.state === 'stopping' && ['preparing', 'running'].includes(accepted.state)) return current;
            if (current.run === accepted.run && current.state === 'terminal' && accepted.state === 'terminal') return current;
            return accepted;
          });
        }
      } catch (cause) {
        if (alive) setPollError(fault(cause));
      } finally {
        if (alive) timer = window.setTimeout(tick, 150);
      }
    }
    async function restore() {
      try {
        const saved = await invoke<Settings>('settings');
        if (!alive) return;
        setSettings(saved);
        setEnvDraft(environmentDraft(saved.ocr_environment));
        setLogLimit(String(saved.gui_log_limit));
        retention.current = saved.gui_log_limit;
        setLogs(old => retainLogs(old, [], saved.gui_log_limit));
        if (saved.package_path) {
          setPackagePath(saved.package_path);
          setOperation('Revalidating remembered package');
          await inspectPath(saved.package_path);
        }
      } catch (cause) {
        if (alive) setError(fault(cause));
      } finally {
        commandInFlight.current = false;
        if (alive) setOperation('');
      }
    }
    void restore();
    void tick();
    return () => {alive = false; window.clearTimeout(timer);};
  }, []);

  async function command(label: string, action: () => Promise<void>) {
    if (commandInFlight.current || closing) return;
    commandInFlight.current = true;
    setOperation(label);
    setError(null);
    setNotice('');
    try {await action();}
    catch (cause) {setError(fault(cause));}
    finally {commandInFlight.current = false; setOperation('');}
  }

  function valuesForCommand() {
    if (!selection) throw new Error('Inspect a package before using its options.');
    if (!bound) throw new Error('This saved profile belongs to a different package or schema. Inspect again; it cannot be rebound.');
    if (numericErrors) throw {category: 'Draft', message: 'Correct the numeric fields before saving, validating, or starting.', context: parsed.errors};
    return parsed.values;
  }

  function selectProfile(id: string) {
    if (!selection) return;
    if (!id) {newDraft(selection); return;}
    const profile = profiles.find(item => item.id === id);
    if (!profile) return;
    setSelectedId(profile.id);
    setName(profile.name);
    setPreset('');
    editDraft(structuredClone(profile.values));
  }

  async function saveProfile() {
    const values = valuesForCommand();
    const savedRevision = revision.current;
    const savedName = name;
    const profile = await invoke<Profile>('save_profile', {id: selectedId, name: savedName, values});
    setProfiles(old => [...old.filter(item => item.id !== profile.id), profile].sort((a, b) => a.name.localeCompare(b.name)));
    setSelectedId(profile.id);
    setPreset('');
    if (savedRevision === revision.current) {
      setDraft(structuredClone(profile.values));
      setValidation(null);
    }
    setNotice(`Saved “${profile.name}” · ${profile.id}. Active runs are unchanged.`);
  }

  async function startRun() {
    if (startInFlight.current || active || commandInFlight.current || closing || startBlock) return;
    let values: Record<string, Json>;
    try {values = valuesForCommand();} catch (cause) {setError(fault(cause)); return;}
    if (!selection) return;
    const profileId = selectedProfile && !valuesDirty ? selectedProfile.id : 'draft';
    const replay = lane === 'replay';
    const request = {
      package_path: packagePath, inventory_identity: selection.inventory_identity,
      package_id: selectedProfile?.package_id ?? selection.package_id,
      schema_identity: selectedProfile?.schema_identity ?? selection.schema_identity,
      profile_id: profileId, values, lane, scenario: replay ? 'workflow' : scenario,
      replay_descriptor_path: replay ? selectedDescriptor : null,
    };
    const profileName = selectedProfile && !valuesDirty ? selectedProfile.name : name || 'Untitled draft';
    startInFlight.current = true;
    epoch.current += 1;
    setStarting(true);
    setError(null);
    setNotice('');
    setDisclosed(false);
    try {
      const run = await invoke<string>('start', {request});
      expectedRun.current = run;
      setSnapshot({kind: 'run', run, lane, packageId: request.package_id, profileName, profileId, scenario: request.scenario, descriptorPath: request.replay_descriptor_path, values});
      setView({...idle, run, state: 'preparing'});
    } catch (cause) {
      setError(fault(cause));
    } finally {
      epoch.current += 1;
      startInFlight.current = false;
      setStarting(false);
    }
  }

  // Check reads saved settings only; the association records exactly what the backend will read.
  async function checkEnvironment() {
    if (startInFlight.current || active || commandInFlight.current || closing || envDirty) return;
    startInFlight.current = true;
    epoch.current += 1;
    setStarting(true);
    setError(null);
    setNotice('');
    setDisclosed(false);
    try {
      const current = settings;
      if (!current?.ocr_environment || !sameEnvironment(current.ocr_environment, envParsed.environment)) {
        throw {category: 'Draft', message: 'Saved settings no longer match this environment. Save the environment, then Check again.', context: null};
      }
      const operation = await invoke<string>('check_environment', {replayDescriptorPath: selectedDescriptor});
      expectedRun.current = operation;
      const association: CheckAssociation = {
        operation, environment: current.ocr_environment, descriptorPath: selectedDescriptor, packageInventoryIdentity: selection?.inventory_identity ?? null,
      };
      setSnapshot({kind: 'check', run: operation, association});
      setView({...idle, run: operation, state: 'preparing', operation: 'environment_check'});
    } catch (cause) {
      setError(fault(cause));
    } finally {
      epoch.current += 1;
      startInFlight.current = false;
      setStarting(false);
    }
  }

  async function stopRun() {
    if (!view.run || stopping || !busyPhases.has(view.state)) return;
    const run = view.run;
    setStopping(true);
    setError(null);
    try {
      await invoke('stop', {run});
      epoch.current += 1;
      setView(current => current.run === run && busyPhases.has(current.state) ? {...current, state: 'stopping'} : current);
      setNotice('Stop requested. Waiting for independent cleanup and terminal evidence.');
    } catch (cause) {setError(fault(cause));}
    finally {setStopping(false);}
  }

  async function saveSettings() {
    if (!/^\d+$/.test(logLimit) || !Number.isSafeInteger(Number(logLimit))) {
      throw new Error('GUI log limit must be an integer from 1 to 10000. The saved setting is unchanged.');
    }
    const previous = await invoke<Settings>('settings');
    const saved = await invoke<Settings>('save_settings', {settings: {...previous, gui_log_limit: Number(logLimit)}});
    setSettings(saved);
    setLogLimit(String(saved.gui_log_limit));
    retention.current = saved.gui_log_limit;
    setLogs(old => retainLogs(old, [], saved.gui_log_limit));
    setNotice(`Saved GUI log limit: ${saved.gui_log_limit}. Run results and file logs are unchanged.`);
  }

  async function saveEnvironment() {
    if (envErrors) throw {category: 'Draft', message: 'Correct the environment fields before saving. The saved environment is unchanged.', context: envParsed.errors};
    const savedRevision = envRevision.current;
    const environment = envParsed.environment;
    const previous = await invoke<Settings>('settings');
    const saved = await invoke<Settings>('save_settings', {settings: {...previous, ocr_environment: environment}});
    setSettings(saved);
    if (savedRevision === envRevision.current) setEnvDraft(environmentDraft(saved.ocr_environment));
    setNotice(saved.ocr_environment
      ? `Saved OCR environment · ${saved.ocr_environment.profile}. Nothing was initialized; run Check. Active operations keep their captured environment.`
      : 'Saved settings with no OCR environment. Controlled runs are unaffected; replay and Check need a saved environment.');
  }

  return <div className="app-shell">
    <header className="app-header"><div><span className="eyebrow">CONTROLLED DESKTOP RUNNER</span><h1>MadoMata</h1></div>
      <div className="header-actions"><span className="lane-badge">QuickJS · recorded replay only · no live authority</span>
        <button id="close" disabled={closing} onClick={async () => {
          setClosing(true);
          try {await getCurrentWindow().close();} catch (cause) {setError(fault(cause)); setClosing(false);}
        }}>{closing ? 'Closing…' : 'Close'}</button>
      </div>
    </header>
    <section className="package-bar" aria-labelledby="package-heading">
      <div className="package-input"><label id="package-heading" htmlFor="package-path">Package directory</label>
        <input id="package-path" type="text" value={packagePath} disabled={locked} placeholder="Absolute path to a package directory" spellCheck={false}
          onChange={event => {setPackagePath(event.target.value); forgetSelection(); setError(null);}}/>
      </div>
      <button id="inspect" disabled={locked || !packagePath.trim()} onClick={() => void command('Inspecting package', () => inspectPath(packagePath))}>Inspect</button>
      <div className="package-identity">{selection ? <><strong>{selection.package_id}</strong><span>{selection.runtime} · inspected</span></> : <span>No inspected package</span>}</div>
    </section>
    <div className="workspace">
      <aside className="sidebar">
        <section aria-labelledby="profiles-heading"><h2 id="profiles-heading">Profiles</h2><p className="muted">Saved locally, outside the package.</p>
          <label htmlFor="profile-select">Saved profile</label>
          <select id="profile-select" value={selectedId ?? ''} disabled={!selection || locked} onChange={event => selectProfile(event.target.value)}>
            <option value="">Unsaved draft</option>{profiles.map(profile => <option key={profile.id} value={profile.id}>{profile.name}</option>)}
          </select>
          <p className={`save-status ${dirty ? 'unsaved' : ''}`}>{selectedProfile ? dirty ? 'Saved profile · draft has changes' : 'Saved · no changes' : 'Draft · not saved'}</p>
          {selectedProfile && <p className="identity-text">{selectedProfile.id}</p>}
          {!bound && <p className="inline-warning">Stale schema identity. This profile cannot be rebound or run.</p>}
          <label htmlFor="profile-name">Profile name</label><input id="profile-name" value={name} disabled={!selection || locked} onChange={event => {setName(event.target.value); setNotice('');}}/>
          <button id="save-profile" className="primary full-width" disabled={!selection || locked || !name.trim() || numericErrors || !bound}
            onClick={() => void command('Saving profile', saveProfile)}>{selectedId ? 'Update profile' : 'Save new profile'}</button>
          <div className="button-row"><button id="new-profile" disabled={!selection || locked} onClick={() => selection && newDraft(selection)}>New draft</button>
            <button id="rename-profile" disabled={!selectedId || locked || !name.trim() || !bound} onClick={() => void command('Renaming profile', async () => {
              const profile = await invoke<Profile>('rename_profile', {id: selectedId, name});
              setProfiles(old => old.map(item => item.id === profile.id ? profile : item));
              setNotice(`Renamed saved profile to “${profile.name}”. Draft values are unchanged.`);
            })}>Rename</button></div>
          <button id="delete-profile" className="danger-text full-width" disabled={!selectedId || locked} onClick={() => void command('Deleting profile', async () => {
            const id = selectedId;
            await invoke('delete_profile', {id});
            setProfiles(old => old.filter(item => item.id !== id));
            setSelectedId(null);
            setNotice('Saved profile deleted. Its values remain in this unsaved draft; active runs are unchanged.');
          })}>Delete profile</button>
          {profilesError && <><FaultMessage title="Some saved profiles could not be loaded" value={profilesError}/><p className="muted">Compatible profiles and new drafts remain available. Stored files were not modified. Restore compatible data, then Inspect again.</p></>}
          <div className="sidebar-divider"/><label htmlFor="preset-select">Package preset</label>
          <select id="preset-select" value={preset} disabled={!selection || locked} onChange={event => selection && newDraft(selection, event.target.value)}>
            <option value="">Top-level defaults</option>{Object.keys(selection?.profiles ?? {}).map(key => <option key={key} value={key}>{key}</option>)}
          </select><p className="muted">Choosing a preset creates a new draft. Save it with a name to keep it.</p>
        </section>
        <section className="settings-panel" aria-labelledby="settings-heading"><h2 id="settings-heading">Display settings</h2>
          <label htmlFor="gui-log-limit">GUI log item limit</label><input id="gui-log-limit" type="text" inputMode="numeric" value={logLimit}
            disabled={locked} onChange={event => setLogLimit(event.target.value)}/>
          <p className="muted">1–10000 · default 1000<br/>Saved limit: {settings?.gui_log_limit ?? 'not loaded'}</p>
          <button id="save-settings" disabled={locked} onClick={() => void command('Saving settings', saveSettings)}>Save settings</button>
        </section>
      </aside>
      <main className="main-content">
        <div className="operation-status" role="status">{operation || notice || 'Edits affect the next operation, never the active snapshot.'}</div>
        <div id="error">{error && <FaultMessage title="Action failed" value={error}/>}
          {primary && (privatePrimary || view.operation === 'environment_check'
            ? <section className="fault" role="alert"><strong>{view.operation === 'environment_check' ? 'Primary check error' : 'Primary run error'} · {faultSummary(primary, !privatePrimary)}</strong>
                <p>Full diagnostics are available through explicit private disclosure below.</p></section>
            : <FaultMessage title="Primary run error" value={primary}/>)}
        </div>
        {pollError && <FaultMessage title="Controller connection failed · last known state retained" value={pollError}/>}
        <div className="editor-run-grid">
          <section className="panel editor-panel" aria-labelledby="draft-heading">
            <div className="panel-heading"><div><span className="eyebrow">NEXT RUN</span><h2 id="draft-heading">{name || 'Untitled draft'}</h2></div><span className="tag">{dirty ? 'Unsaved changes' : 'Saved values'}</span></div>
            <p className="muted">Missing top-level fields use schema defaults. Explicit objects are never recursively filled.</p>
            {selection ? <SchemaForm schema={selection.schema} value={draft} onChange={editDraft} errors={parsed.errors}/>
              : <div className="empty-state"><h3>Inspect a package to begin</h3><p>Choose its directory above. Inspection loads the schema and compatible saved profiles without executing the script.</p></div>}
            <div className="editor-footer"><button id="validate" disabled={!selection || locked || numericErrors || !bound} onClick={() => void command('Validating draft', async () => {
              const checkedRevision = revision.current;
              const effective = await invoke<Record<string, Json>>('validate', {values: valuesForCommand()});
              if (checkedRevision === revision.current) setValidation(effective);
              setNotice(checkedRevision === revision.current ? 'Draft validated by the backend. Start will validate again.' : 'An earlier draft was validated. Current edits still need validation.');
            })}>Validate</button><span className="muted">Backend validation reports exact field paths.</span></div>
            {validation && <details className="validated"><summary>Valid · effective values</summary><pre>{JSON.stringify(validation, null, 2)}</pre></details>}
          </section>
          <section className="panel run-panel" aria-labelledby="run-heading">
            <div className="panel-heading"><div><span className="eyebrow">IMMUTABLE OPERATION</span><h2 id="run-heading">{view.operation === 'environment_check' ? 'Environment check' : 'Execution'}</h2></div>
              <span id="state" className={`phase phase-${starting ? 'preparing' : view.state}`}>{starting ? 'preparing' : view.state}</span></div>
            <label htmlFor="lane">Execution lane</label><select id="lane" value={lane} onChange={event => {setLane(event.target.value); setNotice('');}}>
              <option value="controlled">Controlled · fixture observations, no recognition</option>
              <option value="replay">Replay · recorded corpus, real recognition, controlled sink</option>
              <option value="native" disabled>Native · refused, no live capture or input</option>
            </select>
            <label htmlFor="scenario">Controlled scenario</label><select id="scenario" value={lane === 'replay' ? 'workflow' : scenario} disabled={lane === 'replay'} onChange={event => setScenario(event.target.value)}>
              <option value="workflow">Workflow</option><option value="held-work">Held work · exercise Stop</option><option value="no-match">No match</option>
            </select><p className="muted">Start uses {selectedProfile && !valuesDirty ? `saved profile “${selectedProfile.name}”` : 'the explicit draft shown here'}{lane === 'replay' ? `, the saved OCR environment, and descriptor ${selectedDescriptor ?? '(none)'}` : ''}. Every Start revalidates files; no check or inspection is reused.</p>
            <div className="run-buttons"><button id="start" className="primary" disabled={!selection || active || locked || numericErrors || !bound || startBlock !== null} onClick={() => void startRun()}>Start</button>
              <button id="stop" className="stop-button" disabled={!view.run || !busyPhases.has(view.state) || view.state === 'stopping' || stopping || starting || closing} onClick={() => void stopRun()}>{stopping ? 'Requesting Stop…' : 'Stop'}</button></div>
            {startBlock && <p className="muted" id="start-block">{startBlock}</p>}
            <p className="authority-note">Submitted does not prove effect; Stop does not prove cleanup. Stop covers checks and runs alike.</p>
            <dl className="run-identity"><dt>Operation ID</dt><dd id="run-id">{view.run ?? 'No operation yet'}</dd>
              <dt>Kind</dt><dd id="operation-kind">{view.run ? view.operation === 'environment_check' ? 'Environment check · no package code' : `Run · ${snapshot?.kind === 'run' && snapshot.run === view.run ? snapshot.lane : text(view.result?.lane) ?? 'unknown'} lane` : 'Idle'}</dd>
              {snapshot?.kind === 'run' && <><dt>Captured profile</dt><dd>{snapshot.profileName} · {snapshot.profileId}</dd><dt>Package / scenario</dt><dd>{snapshot.packageId} / {snapshot.scenario}</dd>
                {snapshot.lane === 'replay' && <><dt>Descriptor</dt><dd>{snapshot.descriptorPath}</dd></>}</>}
              {snapshot?.kind === 'check' && <><dt>Checked profile</dt><dd>{snapshot.association.environment?.profile ?? 'unconfigured'}</dd>
                <dt>Descriptor</dt><dd>{snapshot.association.descriptorPath ?? 'none · initialization not attempted'}</dd></>}
            </dl>
            {snapshot?.kind === 'run' && <details><summary>Captured options · unchanged by draft edits</summary><pre>{JSON.stringify(snapshot.values, null, 2)}</pre></details>}
            {snapshot?.kind === 'check' && <details><summary>Captured saved environment · unchanged by later edits</summary><pre>{JSON.stringify(snapshot.association.environment, null, 2)}</pre></details>}
            <h3>Progress milestones</h3><ol className="progress-list">{view.progress.map((event, index) => <li key={`${view.run}-${index}`}>
              <details><summary>{String(event.event ?? 'Milestone')}{text(event.stage) ? ` · ${event.stage}` : ''}{typeof event.at_us === 'number' ? ` · ${(event.at_us / 1000).toFixed(1)} ms` : ''}</summary><pre>{JSON.stringify(event, null, 2)}</pre></details>
            </li>)}</ol>{view.progress.length === 0 && <p className="muted">No milestones recorded.</p>}
            <ResultPanel view={view} disclosed={disclosed} onDisclose={setDisclosed}/>
          </section>
        </div>
        <EnvironmentPanel draft={envDraft} errors={envParsed.errors} onDraft={editEnvironment} saved={savedEnvironment} loaded={settings !== null}
          dirty={envDirty} locked={locked} active={active} descriptorPath={descriptorPath} onDescriptorPath={path => {setDescriptorPath(path); setNotice('');}}
          onSave={() => void command('Saving environment', saveEnvironment)} onCheck={() => void checkEnvironment()} lastCheck={lastCheck} stale={checkStale}/>
        <section className="panel logs-panel" aria-labelledby="logs-heading">
          <div className="panel-heading"><div><span className="eyebrow">STRUCTURED EVENT STREAM</span><h2 id="logs-heading">Logs</h2></div><span className="tag">{logs.items.length} / {settings?.gui_log_limit ?? 1000} retained</span></div>
          <dl className="loss-counters"><div><dt>Display evicted</dt><dd>{logs.evicted}</dd></div><div><dt>Source / controller dropped · current run</dt><dd>{view.dropped_logs}</dd></div>
            <div><dt>GUI transport dropped</dt><dd>{losses.gui_dropped}</dd></div><div><dt>File queue dropped</dt><dd>{losses.file_dropped}</dd></div><div><dt>File I/O errors</dt><dd>{losses.file_errors}</dd></div></dl>
          {losses.last_file_error && <p className="inline-warning">Last file error: {losses.last_file_error}</p>}
          <p className="muted">Newest first. Display eviction does not delete file records or change retained results.</p>
          <ol id="log-list" className="log-list">{[...logs.items].reverse().map(entry => <li className={`log-entry level-${entry.level.toLowerCase()}`} key={entry.sequence}>
            <div className="log-meta"><span className="log-level">{entry.level}</span><time dateTime={new Date(entry.time_ms).toISOString()}>{new Date(entry.time_ms).toLocaleTimeString()}</time><span>#{entry.sequence}</span><span>{entry.source}</span><code>{entry.run ?? 'application'}</code></div>
            <p><code>{entry.code}</code> {entry.message}</p><details><summary>Diagnostic fields</summary><pre>{JSON.stringify(entry.fields, null, 2)}</pre></details>
          </li>)}</ol>{logs.items.length === 0 && <p className="muted">Waiting for structured application and script events.</p>}
        </section>
      </main>
    </div>
  </div>;
}

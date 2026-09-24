import {useEffect, useMemo, useReducer, useRef, useState} from 'react';
import type {KeyboardEvent, ReactNode} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {getCurrentWindow} from '@tauri-apps/api/window';
import {LocalFault, messages, renderMessage} from './i18n.ts';
import type {Command, Message} from './i18n.ts';
import {LocaleContext, useLocale} from './locale.tsx';
import type {CheckTarget, LastCheck} from './settings/EnvironmentPanel.tsx';
import BootstrapPage from './pages/BootstrapPage.tsx';
import GuidancePage from './pages/GuidancePage.tsx';
import LogsPage from './pages/LogsPage.tsx';
import type {Losses} from './pages/LogsPage.tsx';
import CreateWorkspaceDialog from './components/CreateWorkspaceDialog.tsx';
import type {CreateDraft} from './components/CreateWorkspaceDialog.tsx';
import Notifications from './components/Notifications.tsx';
import ResultPanel, {FaultMessage, fault} from './components/ResultPanel.tsx';
import RunPage from './pages/RunPage.tsx';
import type {RunHandlers, RunSnapshot, RunView} from './pages/RunPage.tsx';
import SavedWorkspacesDialog from './components/SavedWorkspacesDialog.tsx';
import Select from './components/Select.tsx';
import SettingsDialog from './settings/SettingsDialog.tsx';
import WorkspaceSwitcher from './components/WorkspaceSwitcher.tsx';
import type {WorkspaceOption} from './components/WorkspaceSwitcher.tsx';
import {INITIAL_BOOTSTRAP, initialSettings, reconstructed, reduceBootstrap, surface} from './bootstrap.ts';
import type {Admission, BootstrapAction} from './bootstrap.ts';
import {dismissCard, emptyStack, ingestCards, interactCard, tickCards, trimCards} from './notifications.ts';
import type {Card, CardStack} from './notifications.ts';
import {DEFAULT_NOTIFICATIONS, acceptController, faultSummary, readEnvironment, readSettingsDraft, retainLogs, retainedCheck, sameEnvironment, sameNotifications, settingsDraftAfterSave, settingsDraftFrom, staleReasons, text} from './state.ts';
import type {CheckAssociation, LogStore, SettingsDraft} from './state.ts';
import {DESCRIPTOR_LIMIT, UNSUPPORTED_SOURCE, WORKSPACE_LIMIT, applyCommand, applyIfCurrent, bindSelection, busy, closeWorkspace, commandValues, deriveBound, ingestResults, isBound, needsAttention, newDraft, originLabel, retainClosed, selectProfile, updateBound, updateWorkspace, workspaceFromView, workspaceLabel, workspaceRef} from './workspace.ts';
import type {Bound, BoundWorkspace, ClosedWorkspace, Derived, LogFilter, LogScope, Origin, ProfileCatalog, RetainedResult, Workspace, WorkspaceCommand} from './workspace.ts';
import type {BootstrapStatus, ControllerView, Fault, Json, LegacyImport, Poll, Profile, Selection, Settings, SnapshotReceipt, StartRequest, TabRecord, WorkspaceCatalog, WorkspaceRef, WorkspaceView} from './types.ts';

const idle: ControllerView = {run: null, state: 'idle', operation: 'run', result: null, error: null, progress: [], dropped_logs: 0, workspace_id: null, workspace_revision: null};
const EMPTY_FILTER: LogFilter = {text: '', level: ''};
const EMPTY_CREATE: CreateDraft = {internalName: '', displayName: ''};

type Nav = {kind: 'none'} | {kind: 'workspace'; id: string} | {kind: 'application'} | {kind: 'closed'; id: string};
interface Operation {run: string; kind: 'run' | 'check'; workspace: WorkspaceRef | null; snapshot: RunSnapshot}
interface Starting {workspaceId: string | null; kind: 'run' | 'check'}
interface DialogError {kind: 'save' | 'check'; value: Fault}

function OperationStrip({idPrefix, owner, kind, phase, run, message, stopDisabled, onStop}: {
  idPrefix: string; owner: string; kind: string; phase: string; run: string | null; message: {text: string; error: boolean} | null; stopDisabled: boolean; onStop: () => void;
}) {
  const locale = useLocale();
  const t = messages[locale].app;
  const ui = messages[locale].ui;
  return <div id={`${idPrefix}-operation-strip`} className="operation-strip" role="status" aria-live="polite">
    <span className="strip-owner"><span className="eyebrow">{t.activeOperation}</span><strong>{owner}</strong></span>
    <span className="strip-kind">{kind} · <code>{run ?? t.admitting}</code></span>
    <span className={`phase phase-${phase}`}>{ui.phase(phase)}</span>
    {message && <span className={message.error ? 'strip-error' : 'strip-note'}>{message.text}</span>}
    <button id={`${idPrefix}-stop`} type="button" className="stop-button" disabled={stopDisabled} onClick={onStop}>{t.stop}</button>
  </div>;
}

export default function App() {
  const [bootstrap, dispatch] = useReducer(reduceBootstrap, INITIAL_BOOTSTRAP);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [closed, setClosed] = useState<ClosedWorkspace[]>([]);
  // Host listing of saved closed Tabs and malformed Tab records; null until the first successful read.
  const [savedClosed, setSavedClosed] = useState<TabRecord[] | null>(null);
  const [catalogFaults, setCatalogFaults] = useState<Fault[]>([]);
  const [catalogError, setCatalogError] = useState<Fault | null>(null);
  const [results, setResults] = useState<Record<string, RetainedResult>>({});
  const [nav, setNav] = useState<Nav>({kind: 'none'});
  const [view, setView] = useState<ControllerView>(idle);
  const [operation, setOperation] = useState<Operation | null>(null);
  const [starting, setStarting] = useState<Starting | null>(null);
  const [stopping, setStopping] = useState(false);
  const [stripMessage, setStripMessage] = useState<{text: Message; error: boolean} | null>(null);
  const [closing, setClosing] = useState(false);
  const [settings, setSettings] = useState<Settings | null>(null);
  // Saved settings win once loaded; before that the temporary bootstrap presentation applies and persists nothing.
  const locale = settings?.locale ?? bootstrap.presentation;
  const t = messages[locale].app;
  const ui = messages[locale].ui;
  const [logs, setLogs] = useState<LogStore>({items: [], evicted: 0});
  const [losses, setLosses] = useState<Losses>({gui_dropped: 0, file_dropped: 0, file_errors: 0, last_file_error: null});
  const [pollError, setPollError] = useState<Fault | null>(null);
  const [lastCheck, setLastCheck] = useState<LastCheck | null>(null);
  const [cards, setCards] = useState<CardStack>(emptyStack);
  const [appBusy, setAppBusy] = useState<Command | null>(null);
  const [pendingClose, setPendingClose] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [settingsDraft, setSettingsDraft] = useState<SettingsDraft>(() => settingsDraftFrom(null));
  const [dialogError, setDialogError] = useState<DialogError | null>(null);
  const [saveNotice, setSaveNotice] = useState<Message | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [createDraft, setCreateDraft] = useState<CreateDraft>(EMPTY_CREATE);
  const [createError, setCreateError] = useState<Fault | null>(null);
  const [savedOpen, setSavedOpen] = useState(false);
  const [configurationOpen, setConfigurationOpen] = useState(false);
  const [reopening, setReopening] = useState<string | null>(null);
  const [reopenError, setReopenError] = useState<Fault | null>(null);
  const [appFilter, setAppFilter] = useState<LogFilter>(EMPTY_FILTER);
  const [appScope, setAppScope] = useState<LogScope>({kind: 'application'});
  const [closedFilter, setClosedFilter] = useState<LogFilter>(EMPTY_FILTER);
  const [reveal, setReveal] = useState<{scope: string; sequence: number} | null>(null);
  const [closedDisclosed, setClosedDisclosed] = useState(false);
  const expectedRun = useRef<string | null>(null);
  const epoch = useRef(0);
  const sessionGeneration = useRef(0);
  const startInFlight = useRef(false);
  // Mirror host admission synchronously so rapid commands cannot cross workspace attribution.
  const hostCommand = useRef<Command | null>(null);
  const bootstrapBusy = useRef(false);
  // Set when a constructing action ended Ready before its catalog could be read; the next catalog decides the rebuild.
  const pendingReconstruction = useRef(false);
  const snapshotBusy = useRef(false);
  const retention = useRef(1000);
  const preferences = useRef(DEFAULT_NOTIFICATIONS);
  const openWorkspaces = useRef(workspaces);
  openWorkspaces.current = workspaces;
  const menuButton = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  const status = bootstrap.status;
  const shell = surface(status);
  const pollEnabled = status?.application_available === true;
  const savedEnvironment = settings?.ocr_environment ?? null;
  const derived = useMemo(() => Object.fromEntries(workspaces.filter(isBound).map(workspace => [workspace.id, deriveBound(workspace.bound, savedEnvironment, locale)])) as Record<string, Derived>, [workspaces, savedEnvironment, locale]);
  const active = starting !== null || busy(view.state);
  const selected = nav.kind === 'workspace' ? workspaces.find(workspace => workspace.id === nav.id) : undefined;
  const closedSelected = nav.kind === 'closed' ? closed.find(item => item.id === nav.id) : undefined;
  const noSelection = nav.kind === 'none' || (nav.kind === 'workspace' && !selected);
  const labelOf = (workspaceId: string | null) => originLabel(workspaceId, workspaces, closed, locale).label;
  // A dirty draft exists only on a bound Tab whose operator touched profile/draft state.
  const dirtyDraft = (workspace: Workspace) => workspace.bound !== null && workspace.bound.touched && derived[workspace.id]?.dirty === true;
  // Settings Save has a separate gate; other pending commands expose their shared refusal reason.
  const busyWorkspace = workspaces.find(workspace => workspace.busy !== null);
  const pendingCommandReason = appBusy !== null && appBusy !== 'savingSettings' ? t[appBusy]
    : bootstrap.pending !== null ? t[bootstrap.pending]
    : busyWorkspace ? `${renderMessage(locale, busyWorkspace.busy)} · ${workspaceLabel(busyWorkspace, workspaces)}`
    : starting ? `${starting.kind === 'check' ? t.admittingCheck : t.admittingRun} · ${labelOf(starting.workspaceId)}`
    : null;
  const normalReady = status?.state === 'ready' && !pendingReconstruction.current;
  const commandReason = pendingCommandReason ?? (normalReady ? null : ui.bootstrap.applicationUnavailable);
  // The bootstrap action in flight disables its own buttons; it is not a foreign command for admission text.
  const admission: Admission = {active, command: (pendingCommandReason !== null && bootstrap.pending === null) || closing, anyDirty: workspaces.some(dirtyDraft)};
  const savedCount = workspaces.length + (savedClosed?.length ?? 0);
  const logCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const entry of logs.items) {
      const key = entry.workspace_id ?? '';
      counts[key] = (counts[key] ?? 0) + 1;
    }
    return counts;
  }, [logs.items]);

  // The live controller belongs to whichever workspace the host attributed it to; others show retained outcomes.
  function runView(workspace: Workspace): RunView {
    if (view.workspace_id === workspace.id && view.run !== null) {
      return {view, live: true, olderRevision: view.workspace_revision !== null && view.workspace_revision !== workspace.revision ? view.workspace_revision : null};
    }
    const retained = results[workspace.id];
    if (retained) return {view: retained.view, live: false, olderRevision: retained.ref.revision !== workspace.revision ? retained.ref.revision : null};
    return {view: idle, live: false, olderRevision: null};
  }

  useEffect(() => {document.documentElement.lang = locale;}, [locale]);

  function adoptSettings(saved: Settings) {
    setSettings(saved);
    dispatch({type:'presentation', locale:saved.locale});
    retention.current = saved.gui_log_limit;
    preferences.current = saved.notifications;
    setLogs(old => retainLogs(old, [], saved.gui_log_limit));
  }

  // A host status is authoritative. Only a constructing action that ended Ready replaces the session: fresh IDs
  // mean a rebuilt Application, so results, the controller view and drafts of the old session are gone.
  function applyStatus(next: BootstrapStatus, constructing: boolean) {
    dispatch({type: 'status', status: next});
    if (next.settings) adoptSettings(next.settings); else setSettings(null);
    const catalog = next.catalog;
    // A constructing action that ended Ready without a readable catalog is confirmed by the next catalog read.
    const rebuild = constructing || pendingReconstruction.current;
    if (catalog === null) {
      pendingReconstruction.current = rebuild && next.state === 'ready';
      return;
    }
    pendingReconstruction.current = false;
    setSavedClosed(catalog.closed);
    setCatalogFaults(catalog.faults);
    setCatalogError(null);
    if (!rebuild || !reconstructed(next, openWorkspaces.current)) return;
    sessionGeneration.current += 1;
    epoch.current += 1;
    setWorkspaces(catalog.open.map(item => workspaceFromView(item)));
    setResults({});
    setClosed([]);
    setLastCheck(null);
    setPollError(null);
    setLogs({items:[], evicted:0});
    setCards(emptyStack);
    setLosses({gui_dropped:0, file_dropped:0, file_errors:0, last_file_error:null});
    setSettingsDraft(settingsDraftFrom(next.settings));
    setDialogOpen(false);
    setCreateOpen(false);
    setSavedOpen(false);
    setConfigurationOpen(false);
    setCreateDraft(EMPTY_CREATE);
    setReveal(null);
    setOperation(null);
    expectedRun.current = null;
    setView(idle);
    setStripMessage(null);
    setPendingClose(null);
    setNav({kind: 'none'});
  }

  async function bootstrapAction(action: BootstrapAction, call: () => Promise<BootstrapStatus>, constructing: boolean) {
    if (bootstrapBusy.current) return;
    bootstrapBusy.current = true;
    dispatch({type: 'pending', action});
    try {
      const next = await call();
      const context = next.fault?.context;
      const installed = next.state === 'ready' || (context !== null && typeof context === 'object' && !Array.isArray(context) && context.configuration_installed === true);
      if ((action === 'restoring' || action === 'recovering') && installed) dispatch({type:'receiptConsumed'});
      applyStatus(next, constructing);
      if (constructing && next.state === 'ready' && next.catalog === null) {
        applyStatus(await invoke<BootstrapStatus>('bootstrap_status'), false);
      }
    } catch (cause) {
      // A refused action throws; the status below stays authoritative and is re-read so Recovery is never hidden.
      dispatch({type: 'actionFailed', fault: fault(cause)});
      try {applyStatus(await invoke<BootstrapStatus>('bootstrap_status'), false);} catch {/* last known status retained */}
    } finally {
      bootstrapBusy.current = false;
      dispatch({type: 'pending', action: null});
    }
  }

  useEffect(() => {
    void bootstrapAction('refreshingStatus', () => invoke<BootstrapStatus>('bootstrap_status'), true);
  }, []);

  // While the host is still loading its root, re-read the status with one timer at a time; `bootstrapBusy` keeps
  // overlapping reads (StrictMode double mount, a slow first load) from racing. The first non-loading status ends it.
  const loading = status?.state === 'loading';
  useEffect(() => {
    if (!loading) return;
    const timer = window.setTimeout(() => void bootstrapAction('refreshingStatus', () => invoke<BootstrapStatus>('bootstrap_status'), true), 300);
    return () => window.clearTimeout(timer);
  }, [loading, status]);

  // Polling starts only once an Application exists and keeps running through a later Recovery so Stop stays reachable.
  useEffect(() => {
    if (!pollEnabled) return;
    let alive = true;
    let timer: number | undefined;
    async function tick() {
      const pollEpoch = epoch.current;
      const pollSession = sessionGeneration.current;
      try {
        const incoming = await invoke<Poll>('poll');
        if (!alive || pollSession !== sessionGeneration.current) return;
        // Drained logs belong to the one shared store, even if the controller snapshot is stale.
        if (incoming.logs.entries.length) {
          setLogs(old => retainLogs(old, incoming.logs.entries, retention.current));
          setCards(old => ingestCards(old, incoming.logs.entries, preferences.current));
        }
        setLosses(old => old.gui_dropped === incoming.logs.gui_dropped && old.file_dropped === incoming.logs.file_dropped
          && old.file_errors === incoming.logs.file_errors && old.last_file_error === incoming.logs.last_file_error ? old : {
            gui_dropped: incoming.logs.gui_dropped, file_dropped: incoming.logs.file_dropped,
            file_errors: incoming.logs.file_errors, last_file_error: incoming.logs.last_file_error,
          });
        setResults(old => ingestResults(old, incoming.workspace_results, openWorkspaces.current));
        const check = incoming.last_check;
        setLastCheck(old => check ? old && old.view.run === check.controller.run && old.view.state === check.controller.state ? old : retainedCheck(check) : null);
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
        if (alive && pollSession === sessionGeneration.current) setPollError(fault(cause));
      } finally {
        if (alive) timer = window.setTimeout(tick, 150);
      }
    }
    void tick();
    return () => {alive = false; window.clearTimeout(timer);};
  }, [pollEnabled]);

  // One countdown for the whole stack; paused cards are skipped inside tickCards.
  const hasCards = cards.cards.length > 0;
  useEffect(() => {
    if (!hasCards) return;
    let last = performance.now();
    const timer = window.setInterval(() => {
      const now = performance.now();
      const elapsed = now - last;
      last = now;
      setCards(old => tickCards(old, elapsed));
    }, 200);
    return () => window.clearInterval(timer);
  }, [hasCards]);

  useEffect(() => {
    if (!menuOpen) return;
    menu.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    function outside(event: MouseEvent) {
      if (!menu.current?.contains(event.target as Node) && !menuButton.current?.contains(event.target as Node)) setMenuOpen(false);
    }
    document.addEventListener('mousedown', outside);
    return () => document.removeEventListener('mousedown', outside);
  }, [menuOpen]);

  function go(next: Nav) {
    setReveal(null);
    setPendingClose(null);
    setClosedDisclosed(false);
    setNav(next);
  }

  function change(id: string, update: (workspace: Workspace) => Workspace) {
    setWorkspaces(list => updateWorkspace(list, id, update));
  }

  // Keep command admission through the follow-up listing; a fresh catalog reaches only the originating Tab.
  async function runCommand(origin: Origin, label: Command, action: () => Promise<WorkspaceCommand>) {
    if (hostCommand.current !== null) return;
    hostCommand.current = label;
    change(origin.id, workspace => ({...workspace, busy: {key: label}, error: null, notice: null}));
    let command: WorkspaceCommand;
    try {
      command = await action();
    } catch (cause) {
      const error = fault(cause);
      command = {update: workspace => ({...workspace, error})};
    } finally {
      hostCommand.current = null;
    }
    setWorkspaces(list => updateWorkspace(applyCommand(list, origin, command), origin.id, workspace => ({...workspace, busy: null})));
  }

  async function readProfiles(workspace: WorkspaceRef): Promise<ProfileCatalog> {
    try {
      return await invoke<ProfileCatalog>('profiles', {workspace});
    } catch (cause) {
      // The mutation already succeeded. Preserve that result without treating a failed read as an empty catalog.
      return {profiles: [], profiles_error: fault(cause)};
    }
  }

  // Package commands need a real inspected selection; an unbound Tab is refused here and again by the host.
  const valuesForCommand = (workspace: Workspace) => commandValues(workspace, workspace.bound ? derived[workspace.id] : undefined);

  // Inspection is scoped to the initiating Tab and publishes only after the host's durable bind.
  function inspectFor(workspace: Workspace) {
    const path = workspace.inspectPath.trim();
    if (!path || commandReason !== null || closing) return;
    const current = runView(workspace);
    if (current.live && busy(current.view.state)) return;
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const ref = workspaceRef(workspace);
    const rebinding = workspace.bound !== null;
    void runCommand(origin, rebinding ? 'reinspectingPackage' : 'inspectingPackage', async () => {
      const selection = await invoke<Selection>('inspect', {packagePath: path, workspace: ref});
      return {update: item => bindSelection(item, selection, {key: rebinding ? 'reinspected' : 'bound'})};
    });
  }

  function handlers(workspace: BoundWorkspace): RunHandlers {
    const ref = workspaceRef(workspace);
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const locked = commandReason !== null || closing;
    const bound = workspace.bound;
    const edit = (update: (bound: Bound) => Bound) => change(workspace.id, item => ({...updateBound(item, update), notice: null}));
    return {
      change: edit,
      disclose: run => change(workspace.id, item => updateBound(item, current => ({...current, disclosedRun: run}))),
      selectProfile: id => edit(current => selectProfile(current, id)),
      newDraft: preset => edit(current => newDraft(current, preset)),
      inspectPath: value => change(workspace.id, item => ({...item, inspectPath: value, error: null})),
      validate: () => {
        if (locked) return;
        const checked = bound.draftRevision;
        void runCommand(origin, 'validatingDraft', async () => {
          const values = valuesForCommand(workspace);
          const effective = await invoke<Record<string, Json>>('validate', {workspace: ref, values});
          return {update: item => item.bound?.draftRevision === checked
            ? {...updateBound(item, current => ({...current, validation: effective})), notice: {key: 'validated'}}
            : {...item, notice: {key: 'earlierValidated'}}};
        });
      },
      saveProfile: () => {
        if (locked) return;
        const checked = bound.draftRevision;
        void runCommand(origin, 'savingProfile', async () => {
          const values = valuesForCommand(workspace);
          const profile = await invoke<Profile>('save_profile', {workspace: ref, id: bound.selectedId, name: bound.name, values});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...updateBound(item, current => ({
            ...current, profiles: [...current.profiles.filter(entry => entry.id !== profile.id), profile].sort((a, b) => a.name.localeCompare(b.name)),
            selectedId: profile.id, preset: '',
            ...(current.draftRevision === checked ? {draft: structuredClone(profile.values), validation: null, touched: false} : {}),
          })), notice: {key: 'profileSaved', args: [profile.name, profile.id]}})};
        });
      },
      renameProfile: () => {
        if (locked || !bound.selectedId) return;
        const id = bound.selectedId;
        void runCommand(origin, 'renamingProfile', async () => {
          const profile = await invoke<Profile>('rename_profile', {workspace: ref, id, name: bound.name});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...updateBound(item, current => ({...current, profiles: current.profiles.map(entry => entry.id === profile.id ? profile : entry)})), notice: {key: 'profileRenamed', args: [profile.name]}})};
        });
      },
      deleteProfile: () => {
        if (locked || !bound.selectedId) return;
        const id = bound.selectedId;
        void runCommand(origin, 'deletingProfile', async () => {
          await invoke('delete_profile', {workspace: ref, id});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...updateBound(item, current => ({...current, profiles: current.profiles.filter(entry => entry.id !== id), selectedId: current.selectedId === id ? null : current.selectedId, touched: true})),
            notice: {key: 'profileDeleted'}})};
        });
      },
      importLegacy: () => {
        if (locked) return;
        void runCommand(origin, 'importingProfiles', async () => {
          const result = await invoke<LegacyImport>('import_legacy_profiles', {workspace: ref});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...item, legacyImport: result,
            notice: {key: result.fault ? 'profilesImportPartial' : 'profilesImported', args: [result.imported.length, result.unchanged.length]}})};
        });
      },
      reinspect: () => inspectFor(workspace),
      start: () => void startRun(workspace),
      stop: () => void stopRun(),
    };
  }

  async function startRun(workspace: BoundWorkspace) {
    const facts = derived[workspace.id];
    const bound = workspace.bound;
    if (hostCommand.current !== null || active || closing || facts.startBlock || facts.descriptorError) return;
    let values: Record<string, Json>;
    try {values = valuesForCommand(workspace);} catch (cause) {const error = fault(cause); change(workspace.id, item => ({...item, error})); return;}
    const profile = facts.selectedProfile;
    const profileId = profile && !facts.valuesDirty ? profile.id : 'draft';
    const replay = bound.lane === 'replay';
    const request: StartRequest = {
      package_path: bound.packagePath, inventory_identity: bound.package.inventory_identity,
      package_id: profile?.package_id ?? bound.package.package_id, schema_identity: profile?.schema_identity ?? bound.package.schema_identity,
      profile_id: profileId, values, lane: bound.lane, scenario: replay ? 'workflow' : bound.scenario,
      replay_descriptor_path: replay ? bound.descriptorPath.trim() || null : null,
    };
    const profileName = profile && !facts.valuesDirty ? profile.name : bound.name;
    const ref = workspaceRef(workspace);
    hostCommand.current = 'admittingRun';
    startInFlight.current = true;
    epoch.current += 1;
    setStarting({workspaceId: workspace.id, kind: 'run'});
    setStripMessage(null);
    change(workspace.id, item => ({...updateBound(item, current => ({...current, disclosedRun: null})), error: null, notice: null}));
    try {
      const run = await invoke<string>('start', {workspace: ref, request});
      expectedRun.current = run;
      setOperation({run, kind: 'run', workspace: ref, snapshot: {kind: 'run', run, lane: bound.lane, packageId: request.package_id, profileName, profileId, scenario: request.scenario, descriptorPath: request.replay_descriptor_path, values}});
      setView({...idle, run, state: 'preparing', workspace_id: ref.workspace_id, workspace_revision: ref.revision});
    } catch (cause) {
      // A refused Start releases only its own preparation state; nothing else changes.
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, {id: workspace.id, revision: workspace.revision}, item => ({...item, error})));
    } finally {
      epoch.current += 1;
      startInFlight.current = false;
      hostCommand.current = null;
      setStarting(null);
    }
  }

  const envParsed = useMemo(() => readEnvironment(settingsDraft.environment, locale), [settingsDraft.environment, locale]);
  const envDirty = Object.keys(envParsed.errors).length > 0 || !sameEnvironment(envParsed.environment, savedEnvironment);
  const parsedSettings = useMemo(() => readSettingsDraft(settingsDraft, locale), [settingsDraft, locale]);
  const dialogDirty = settings === null || settingsDraft.locale !== settings.locale || settingsDraft.logLimit.trim() !== String(settings.gui_log_limit)
    || !sameNotifications(settingsDraft.notifications, settings.notifications) || settingsDraft.backupDirectory.trim() !== (settings.backup_directory ?? '') || envDirty;
  // Check binds the workspace visible when the dialog opened; an unbound Tab is never passed as a package association.
  const checkTarget: CheckTarget = selected?.bound
    ? {workspace: workspaceRef(selected), label: workspaceLabel(selected, workspaces), descriptorPath: selected.bound.descriptorPath.trim() || null, packageInventoryIdentity: selected.bound.package.inventory_identity}
    : {workspace: null, label: t.none, descriptorPath: null, packageInventoryIdentity: null};
  const checkStale = lastCheck ? staleReasons(lastCheck.association, {
    saved: savedEnvironment, draftDirty: envDirty, workspace: checkTarget.workspace, descriptorPath: checkTarget.descriptorPath, packageInventoryIdentity: checkTarget.packageInventoryIdentity,
  }, locale) : [];

  // Check reads saved settings only; the association records exactly what the backend will read.
  async function checkEnvironment() {
    if (hostCommand.current !== null || active || closing || envDirty || appBusy) return;
    if (selected?.bound && derived[selected.id].descriptorError) {
      setDialogError({kind: 'check', value: new LocalFault({key: 'descriptorLimit', args: [DESCRIPTOR_LIMIT]})});
      return;
    }
    const target = checkTarget;
    hostCommand.current = 'admittingCheck';
    startInFlight.current = true;
    epoch.current += 1;
    setStarting({workspaceId: target.workspace?.workspace_id ?? null, kind: 'check'});
    setStripMessage(null);
    setDialogError(null);
    try {
      const current = settings;
      if (!current?.ocr_environment || !sameEnvironment(current.ocr_environment, envParsed.environment)) {
        throw new LocalFault({key: 'environmentChanged'});
      }
      const run = await invoke<string>('check_environment', {workspace: target.workspace, replayDescriptorPath: target.descriptorPath});
      expectedRun.current = run;
      const association: CheckAssociation = {operation: run, workspace: target.workspace, environment: current.ocr_environment, descriptorPath: target.descriptorPath, packageInventoryIdentity: target.packageInventoryIdentity};
      setOperation({run, kind: 'check', workspace: target.workspace, snapshot: {kind: 'check', run, association}});
      setView({...idle, run, state: 'preparing', operation: 'environment_check', workspace_id: target.workspace?.workspace_id ?? null, workspace_revision: target.workspace?.revision ?? null});
    } catch (cause) {
      setDialogError({kind: 'check', value: fault(cause)});
    } finally {
      epoch.current += 1;
      startInFlight.current = false;
      hostCommand.current = null;
      setStarting(null);
    }
  }

  // Stop addresses the retained operation ID, never the visible workspace, and never waits on workspace state.
  async function stopRun() {
    if (!view.run || stopping || !busy(view.state)) return;
    const run = view.run;
    setStopping(true);
    try {
      await invoke('stop', {run});
      epoch.current += 1;
      setView(current => current.run === run && busy(current.state) ? {...current, state: 'stopping'} : current);
      setStripMessage({text: {key: 'stopRequested'}, error: false});
    } catch (cause) {
      setStripMessage({text: {key: 'stopFailed', args: [faultSummary(fault(cause), true)]}, error: true});
    } finally {
      setStopping(false);
    }
  }

  function openSettings() {
    if (settings === null || !normalReady) return;
    menuButton.current?.focus();
    setMenuOpen(false);
    setSettingsDraft(settingsDraftFrom(settings));
    setDialogError(null);
    setSaveNotice(null);
    setDialogOpen(true);
  }

  async function saveSettings() {
    const editable = parsedSettings.settings;
    if (settings === null || !normalReady || !editable || appBusy || commandReason !== null) return;
    const submitted = settingsDraft;
    setAppBusy('savingSettings');
    setDialogError(null);
    try {
      const saved = await invoke<Settings>('save_settings', {settings: editable});
      adoptSettings(saved);
      setCards(old => trimCards(old, saved.notifications.visible_count));
      setSettingsDraft(current => settingsDraftAfterSave(current, submitted, saved));
      setSaveNotice(saved.ocr_environment ? {key: 'settingsSaved', args: [saved.ocr_environment.profile]} : {key: 'settingsSavedEmpty'});
    } catch (cause) {
      setDialogError({kind: 'save', value: fault(cause)});
      // A settings fault after Ready is recorded by the shell; re-reading it exposes Recovery without dropping the Application.
      void bootstrapAction('refreshingStatus', () => invoke<BootstrapStatus>('bootstrap_status'), false);
    } finally {
      setAppBusy(null);
    }
  }

  // The dispatched archive keeps its owner and result; dismissing a dialog neither cancels nor rolls it back.
  async function snapshot(destination: string | null) {
    if (snapshotBusy.current) return;
    snapshotBusy.current = true;
    dispatch({type: 'snapshotPending'});
    try {
      const receipt = await invoke<SnapshotReceipt>('snapshot', {destination});
      dispatch({type: 'snapshotSettled', outcome: {kind: 'receipt', receipt}});
    } catch (cause) {
      dispatch({type: 'snapshotSettled', outcome: {kind: 'fault', fault: fault(cause)}});
    } finally {
      snapshotBusy.current = false;
    }
  }

  const receiptGeneration = bootstrap.receiptGeneration;
  const bootstrapHandlers = {
    onInitialize: () => void bootstrapAction('initializing', () => invoke<BootstrapStatus>('initialize', {settings: initialSettings(bootstrap.setup), confirmFresh: status?.legacy_root !== null && bootstrap.setup.startFresh}), true),
    onRetry: () => void bootstrapAction('retrying', () => invoke<BootstrapStatus>('retry_bootstrap', {discard: bootstrap.restore.discard}), true),
    onImportRoot: () => void bootstrapAction('importingRoot', () => invoke<BootstrapStatus>('import_legacy_root'), true),
    onSnapshot: () => void snapshot(bootstrap.snapshotDestination.trim() || null),
    onRestore: () => void bootstrapAction('restoring', () => invoke<BootstrapStatus>('restore_snapshot', {
      archivePath: bootstrap.restore.archivePath.trim(), receiptGeneration, confirm: bootstrap.restore.confirm, discard: bootstrap.restore.discard,
    }), true),
    onRecover: (rollback: boolean) => void bootstrapAction('recovering', () => invoke<BootstrapStatus>('recover_restore', {rollback, confirm: bootstrap.restore.recoverConfirm, discard: bootstrap.restore.discard}), true),
    onExit: () => void exitApplication(),
    onDismiss: () => {setConfigurationOpen(false); menuButton.current?.focus();},
  };

  async function exitApplication() {
    setMenuOpen(false);
    setClosing(true);
    try {await getCurrentWindow().close();} catch (cause) {dispatch({type: 'actionFailed', fault: fault(cause)}); setClosing(false);}
  }

  async function refreshSaved() {
    try {
      const catalog = await invoke<WorkspaceCatalog>('workspace_catalog');
      setSavedClosed(catalog.closed);
      setCatalogFaults(catalog.faults);
      setCatalogError(null);
    } catch (cause) {
      setCatalogError(fault(cause));
    }
  }

  function openCreate() {
    if (appBusy !== null || closing || !normalReady) return;
    setCreateError(null);
    setCreateOpen(true);
  }

  function openSaved() {
    if (closing || !normalReady) return;
    setReopenError(null);
    setSavedOpen(true);
    void refreshSaved();
  }

  // Creation persists a genuinely unbound Tab; success navigates to its Edit guidance, failure keeps the draft.
  async function createWorkspace() {
    if (hostCommand.current !== null || appBusy !== null || closing || !normalReady) return;
    hostCommand.current = 'creatingWorkspace';
    setAppBusy('creatingWorkspace');
    setCreateError(null);
    try {
      const created = await invoke<WorkspaceView>('create_workspace', {internalName: createDraft.internalName, displayName: createDraft.displayName});
      setWorkspaces(list => [...list, workspaceFromView(created, {key: 'created'})]);
      go({kind: 'workspace', id: created.workspace_id});
      setCreateOpen(false);
      setCreateDraft(EMPTY_CREATE);
      void refreshSaved();
    } catch (cause) {
      setCreateError(fault(cause));
    } finally {
      hostCommand.current = null;
      setAppBusy(null);
    }
  }

  async function reopenWorkspace(internalName: string) {
    if (hostCommand.current !== null || appBusy !== null || closing || !normalReady) return;
    hostCommand.current = 'reopeningWorkspace';
    setAppBusy('reopeningWorkspace');
    setReopening(internalName);
    setReopenError(null);
    try {
      const reopened = await invoke<WorkspaceView>('reopen_workspace', {internalName});
      setWorkspaces(list => [...list, workspaceFromView(reopened, {key: 'reopened'})]);
      go({kind: 'workspace', id: reopened.workspace_id});
      setSavedOpen(false);
      void refreshSaved();
    } catch (cause) {
      setReopenError(fault(cause));
    } finally {
      hostCommand.current = null;
      setAppBusy(null);
      setReopening(null);
    }
  }

  function focusWorkspaceSelection(remaining: number) {
    requestAnimationFrame(() => {
      (remaining > 0 ? document.getElementById('workspace-select') : document.getElementById('new-workspace'))?.focus();
    });
  }

  async function closeTab(workspace: Workspace, confirmed: boolean) {
    const current = runView(workspace);
    if (hostCommand.current !== null) {const pending = hostCommand.current; change(workspace.id, item => ({...item, notice: {key: 'waitForCommand', args: [pending]}})); return;}
    if (current.live && busy(current.view.state)) {change(workspace.id, item => ({...item, notice: {key: 'ownerCannotClose'}})); return;}
    if (dirtyDraft(workspace) && !confirmed) {setPendingClose(workspace.id); return;}
    setPendingClose(null);
    hostCommand.current = 'closingWorkspace';
    change(workspace.id, item => ({...item, busy: {key: 'closingWorkspace'}, error: null}));
    let remaining = workspaces.length;
    try {
      await invoke('close_workspace', {workspace: workspaceRef(workspace)});
      const index = workspaces.findIndex(item => item.id === workspace.id);
      const neighbor = workspaces[index + 1] ?? workspaces[index - 1];
      remaining = workspaces.length - 1;
      setClosed(old => retainClosed(old, {id: workspace.id, revision: workspace.revision, label: workspaceLabel(workspace, workspaces), result: current.view.run ? current.view : null}));
      setWorkspaces(list => closeWorkspace(list, workspace.id));
      setResults(old => {
        if (!(workspace.id in old)) return old;
        const next = {...old};
        delete next[workspace.id];
        return next;
      });
      if (nav.kind === 'workspace' && nav.id === workspace.id) go(neighbor ? {kind: 'workspace', id: neighbor.id} : {kind: 'none'});
      void refreshSaved();
    } catch (cause) {
      const error = fault(cause);
      change(workspace.id, item => ({...item, busy: null, error}));
    } finally {
      hostCommand.current = null;
      focusWorkspaceSelection(remaining);
    }
  }

  function openDiagnostics(card: Card) {
    setCards(old => dismissCard(old, card.id));
    if (card.workspaceId === null) {
      setAppFilter(EMPTY_FILTER);
      go({kind: 'application'});
    } else if (workspaces.some(workspace => workspace.id === card.workspaceId)) {
      const id = card.workspaceId;
      change(id, item => ({...item, page: 'logs', logFilter: EMPTY_FILTER}));
      go({kind: 'workspace', id});
    } else {
      setClosedFilter(EMPTY_FILTER);
      go({kind: 'closed', id: card.workspaceId});
    }
    setReveal({scope: card.workspaceId ?? 'application', sequence: card.id});
  }

  function menuKeys(event: KeyboardEvent<HTMLDivElement>) {
    const items = Array.from(menu.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    const index = items.indexOf(document.activeElement as HTMLElement);
    if (event.key === 'Escape') {event.preventDefault(); setMenuOpen(false); menuButton.current?.focus();}
    else if (event.key === 'ArrowDown') {event.preventDefault(); items[(index + 1) % items.length]?.focus();}
    else if (event.key === 'ArrowUp') {event.preventDefault(); items[(index - 1 + items.length) % items.length]?.focus();}
    else if (event.key === 'Tab') setMenuOpen(false);
  }

  const owner = starting ? starting.workspaceId : view.workspace_id;
  const phase = starting ? 'preparing' : view.state;
  const stripKind = ui.operation((starting?.kind ?? operation?.kind ?? (view.operation === 'environment_check' ? 'check' : 'run')) === 'check' ? 'environment_check' : 'run');
  const strip = (idPrefix: string): ReactNode => active ? <OperationStrip idPrefix={idPrefix} owner={owner === null ? t.applicationOwner : labelOf(owner)} kind={stripKind} phase={phase} run={starting ? null : view.run}
    message={stripMessage && {text: renderMessage(locale, stripMessage.text), error: stripMessage.error}} stopDisabled={!view.run || !busy(view.state) || view.state === 'stopping' || stopping || starting !== null || closing} onStop={() => void stopRun()}/> : null;

  if (shell !== 'shell') {
    // Before the first resolved status the shell is already real: presentation choice and Exit work, nothing is assumed.
    return <LocaleContext value={locale}>
      {shell === 'loading' || status === null
        ? <div className="bootstrap-page">
          <header className="topbar">
            <div className="brand"><span className="brandmark" aria-hidden="true">M</span><span>MadoMata</span></div>
            <div className="topbar-actions"><div className="inline-label"><label htmlFor="presentation-locale">{ui.bootstrap.presentation}</label>
              <Select id="presentation-locale" value={bootstrap.presentation} options={[{value: 'en', label: ui.settings.languageNames.en}, {value: 'ja', label: ui.settings.languageNames.ja}]}
                onChange={value => {if (value === 'en' || value === 'ja') dispatch({type: 'presentation', locale: value});}}/></div>
              <button id="bootstrap-exit" type="button" disabled={closing} onClick={() => void exitApplication()}>{closing ? ui.bootstrap.exiting : ui.bootstrap.exit}</button></div>
          </header>
          <main className="content bootstrap-content" aria-busy="true">
            <p className="muted" role="status">{ui.bootstrap.loading}</p>
            {bootstrap.actionError && <FaultMessage title={ui.bootstrap.actionFailed} value={bootstrap.actionError}/>}
          </main>
        </div>
        : <BootstrapPage ui={bootstrap} status={status} dispatch={dispatch} handlers={bootstrapHandlers} admission={admission} exiting={closing} inShell={false} strip={null}/>}
    </LocaleContext>;
  }

  const workspaceItems: WorkspaceOption[] = workspaces.map(workspace => {
    const current = runView(workspace);
    const facts = derived[workspace.id];
    const owns = starting?.workspaceId === workspace.id || (current.live && busy(current.view.state));
    const sourceIssue = workspace.sourceError ? workspace.sourceError.category === UNSUPPORTED_SOURCE ? t.unsupportedSource : t.unavailableSource : null;
    const issue = workspace.error?.category ?? workspace.bound?.profilesError?.category ?? sourceIssue
      ?? (facts ? facts.numericErrors ? t.invalidFields : !facts.bound ? t.staleProfile : facts.descriptorError ? t.invalidDescriptor : null : null);
    const attention = issue !== null || needsAttention(current.view);
    const optionStatus: WorkspaceOption['status'] = owns ? {kind: 'busy', text: `${ui.phase(starting?.workspaceId === workspace.id ? 'preparing' : current.view.state)} · ${ui.operation(starting?.workspaceId === workspace.id ? starting.kind === 'check' ? 'environment_check' : 'run' : current.view.operation)}`}
      : workspace.busy ? {kind: 'busy', text: renderMessage(locale, workspace.busy)}
      : attention ? {kind: 'attention', text: t.attention(issue ?? current.view.error?.category ?? text(current.view.result?.status) ?? t.unresolved)}
      : dirtyDraft(workspace) ? {kind: 'dirty', text: t.unsavedDraft}
      : facts ? {kind: 'ready', text: t.ready(facts.selectedProfile ? facts.selectedProfile.name : t.draft)}
      : {kind: 'ready', text: t.noPackage};
    return {id: workspace.id, title: workspace.bound?.packagePath ?? workspace.internalName, label: workspaceLabel(workspace, workspaces), status: optionStatus, running: owns, attention};
  });
  const inShellRecovery = status !== null && status.state === 'recovery';
  const foreignOperation = (workspace: Workspace) => active && owner !== workspace.id;

  return <LocaleContext value={locale}><div className="app">
    <header className="topbar">
      <div className="brand"><span className="brandmark" aria-hidden="true">M</span><span>MadoMata</span><span className="divider" aria-hidden="true"/><span className="eyebrow">{t.workspace}</span></div>
      <div className="topbar-actions">
        <span className="lane-badge">{t.controlledScope}</span>
        <div className="menu-anchor">
          <button id="application-menu" ref={menuButton} type="button" aria-haspopup="menu" aria-expanded={menuOpen} aria-controls="application-menu-items" onClick={() => setMenuOpen(open => !open)}>{t.application} ▾</button>
          {menuOpen && <div id="application-menu-items" ref={menu} className="dropdown" role="menu" aria-labelledby="application-menu" onKeyDown={menuKeys}>
            <button type="button" role="menuitem" id="menu-settings" disabled={settings === null || !normalReady} onClick={openSettings}>{t.appSettings}</button>
            <button type="button" role="menuitem" id="menu-restore" onClick={() => {menuButton.current?.focus(); setMenuOpen(false); setConfigurationOpen(true);}}>{ui.bootstrap.restoreHeading}</button>
            <button type="button" role="menuitem" id="menu-application-logs" aria-current={nav.kind === 'application' ? 'page' : undefined} onClick={() => {setMenuOpen(false); go({kind: 'application'});}}>{t.applicationLogs}<span className="count">{logCounts[''] ?? 0}</span></button>
            <button type="button" role="menuitem" id="close" disabled={closing} onClick={() => void exitApplication()}>{closing ? t.closing : t.closeWindow}</button>
            <p className="menu-description">{t.menuDescription}</p>
          </div>}
        </div>
      </div>
    </header>
    <div className="workspace-bar">
      <div className="workspace-switcher">
        <WorkspaceSwitcher items={workspaceItems} selectedId={selected?.id ?? null} onSelect={id => go({kind: 'workspace', id})}
          onClose={selected ? () => void closeTab(selected, false) : null}
          closeReason={active && owner === selected?.id ? t.ownsOperation : commandReason ?? (closing ? t.applicationClosing : null)}
          onSaved={openSaved} savedDisabled={closing || !normalReady}/>
        <button id="new-workspace" type="button" className="workspace-action" aria-label={t.newWorkspace} aria-haspopup="dialog" disabled={appBusy !== null || closing || !normalReady}
          title={workspaces.length >= WORKSPACE_LIMIT ? t.workspaceLimit(WORKSPACE_LIMIT) : t.newWorkspace} onClick={openCreate}>+</button>
        <span id="workspace-summary" className="visually-hidden">{t.workspaceSummary}</span>
      </div>
      <nav className="workspace-pages" aria-label={selected ? t.selectedPages : t.currentScope}>
        {selected && <>
          <button id="page-run" type="button" className="nav-item" aria-current={selected.page === 'run' ? 'page' : undefined} onClick={() => {setReveal(null); change(selected.id, item => ({...item, page: 'run'}));}}>{t.runControl}</button>
          <button id="page-logs" type="button" className="nav-item" aria-current={selected.page === 'logs' ? 'page' : undefined} onClick={() => {setReveal(null); change(selected.id, item => ({...item, page: 'logs'}));}}>{t.logs}<span className="count">{logCounts[selected.id] ?? 0}</span></button>
        </>}
        {nav.kind === 'application' && <span className="scope-label">{t.applicationLogs}</span>}
        {nav.kind === 'closed' && <span className="scope-label">{t.closedDiagnostics}</span>}
      </nav>
    </div>
    {pendingClose && workspaces.some(workspace => workspace.id === pendingClose) && <div className="confirm-bar" role="alertdialog" aria-labelledby="confirm-close-text">
      <span id="confirm-close-text">{t.closeConfirm(labelOf(pendingClose))}</span>
      <button type="button" className="danger-text" onClick={() => {const workspace = workspaces.find(item => item.id === pendingClose); if (workspace) void closeTab(workspace, true);}}>{t.discardClose}</button>
      <button type="button" autoFocus onClick={() => {setPendingClose(null); focusWorkspaceSelection(workspaces.length);}}>{t.keepOpen}</button>
    </div>}
    {strip('app')}
    {pollError && <div className="content-wide"><FaultMessage title={t.connectionFailed} value={pollError}/></div>}
    {(inShellRecovery || configurationOpen) && status && <BootstrapPage ui={bootstrap} status={status} dispatch={dispatch} handlers={bootstrapHandlers} admission={admission} exiting={closing} inShell={true} strip={null}/>}
    {status?.state === 'ready' && status.fault && <div className="content-wide"><FaultMessage title={`${ui.bootstrap.stage} · ${status.stage}`} value={status.fault}/>
      <div className="button-row"><button id="refresh-status" type="button" disabled={bootstrap.pending !== null} onClick={() => void bootstrapAction('refreshingStatus', () => invoke<BootstrapStatus>('bootstrap_status'), false)}>{ui.reopen.refresh}</button>
        <span className="muted">{ui.bootstrap.actionFailedHelp}</span></div></div>}
    <div className="workspace">
      <main id="workspace-panel" className="content" aria-label={selected ? t.workspaceAria(workspaceLabel(selected, workspaces)) : undefined}>
        {selected && selected.page === 'run' && (isBound(selected)
          ? <RunPage key={`${selected.id}:${selected.revision}`} workspace={selected} label={workspaceLabel(selected, workspaces)} derived={derived[selected.id]} run={runView(selected)}
            snapshot={operation && operation.workspace?.workspace_id === selected.id ? operation.snapshot : null}
            locked={commandReason !== null || closing} active={active} starting={starting?.workspaceId === selected.id} stopping={stopping} closing={closing}
            savedEnvironment={savedEnvironment} handlers={handlers(selected)}/>
          : <GuidancePage workspace={selected} label={workspaceLabel(selected, workspaces)} locked={commandReason !== null || closing} lockReason={commandReason ?? (closing ? t.applicationClosing : null)}
            onPath={value => change(selected.id, item => ({...item, inspectPath: value, error: null}))} onInspect={() => inspectFor(selected)} foreignOperation={foreignOperation(selected)}/>)}
        {selected && selected.page === 'logs' && <LogsPage eyebrow={t.activity(workspaceLabel(selected, workspaces))} heading={t.logs} description={t.workspaceLogHelp}
          items={logs.items} evicted={logs.evicted} limit={settings?.gui_log_limit ?? retention.current} scope={{kind: 'workspace', id: selected.id}}
          filter={selected.logFilter} onFilter={filter => {setReveal(null); change(selected.id, item => ({...item, logFilter: filter}));}}
          losses={losses} sourceDropped={runView(selected).live ? runView(selected).view.dropped_logs : null}
          reveal={reveal?.scope === selected.id ? reveal.sequence : null} originLabel={labelOf} showOrigin={false}/>}
        {nav.kind === 'application' && <LogsPage eyebrow={t.appActivity} heading={t.applicationLogs} description={t.appLogHelp}
          items={logs.items} evicted={logs.evicted} limit={settings?.gui_log_limit ?? retention.current} scope={appScope} onScope={scope => {setReveal(null); setAppScope(scope);}}
          filter={appFilter} onFilter={filter => {setReveal(null); setAppFilter(filter);}} losses={losses} sourceDropped={view.run ? view.dropped_logs : null}
          reveal={reveal?.scope === 'application' ? reveal.sequence : null} originLabel={labelOf} showOrigin={appScope.kind === 'all'}/>}
        {nav.kind === 'closed' && <>
          <section className="panel closed-notice"><div className="panel-body">
            <span className="eyebrow">{t.closedWorkspace}</span>
            <h2>{t.closedLabel(closedSelected ? closedSelected.label : nav.id)}</h2>
            <p className="muted">{t.closedHelp}{!closedSelected && <> {t.closedEvicted}</>}</p>
            {closedSelected?.result && <><h3>{t.retainedOutcome(closedSelected.revision)}</h3><ResultPanel view={closedSelected.result} disclosed={closedDisclosed} onDisclose={setClosedDisclosed}/></>}
          </div></section>
          <LogsPage eyebrow={t.closedActivity} heading={t.retainedEvents} description={t.closedLogHelp}
            items={logs.items} evicted={logs.evicted} limit={settings?.gui_log_limit ?? retention.current} scope={{kind: 'workspace', id: nav.id}}
            filter={closedFilter} onFilter={filter => {setReveal(null); setClosedFilter(filter);}} losses={losses} sourceDropped={null}
            reveal={reveal?.scope === nav.id ? reveal.sequence : null} originLabel={labelOf} showOrigin={false}/>
        </>}
        {noSelection && <>
          <div className="page-heading"><div><span className="eyebrow">{t.workspace}</span><h1>{workspaces.length === 0 ? t.noWorkspaces : t.chooseWorkspace}</h1>
            <p>{workspaces.length === 0 ? t.firstWorkspace : t.chooseHelp}</p></div>
            <div className="actions">
              <button id="create-first" type="button" className="primary" disabled={appBusy !== null || closing || !normalReady} onClick={openCreate}>{ui.workspaces.new}</button>
              <button id="saved-first" type="button" disabled={closing || !normalReady} onClick={openSaved}>{ui.workspaces.saved}</button>
            </div></div>
          {catalogFaults.length > 0 && <section className="panel catalog-faults"><div className="panel-body"><h2>{ui.bootstrap.catalogFaults}</h2><p className="muted">{ui.bootstrap.catalogFaultsHelp}</p>
            {catalogFaults.map((item, index) => <FaultMessage key={index} title={ui.bootstrap.catalogFaults} value={item}/>)}</div></section>}
        </>}
      </main>
    </div>
    <Notifications cards={cards.cards} scopeLabel={labelOf} onDismiss={id => setCards(old => dismissCard(old, id))} onOpen={openDiagnostics}
      onInteract={(id, interaction) => setCards(old => interactCard(old, id, interaction))}/>
    <CreateWorkspaceDialog open={createOpen} draft={createDraft} onDraft={next => {setCreateDraft(next); setCreateError(null);}} onCancel={() => {if (appBusy !== 'creatingWorkspace') setCreateOpen(false);}}
      onCreate={() => void createWorkspace()} creating={appBusy === 'creatingWorkspace'} error={createError} openCount={workspaces.length} savedCount={savedCount}
      busyReason={appBusy === 'creatingWorkspace' ? null : commandReason} strip={strip('create')}/>
    <SavedWorkspacesDialog open={savedOpen} onCancel={() => {if (reopening === null) setSavedOpen(false);}} closed={savedClosed} faults={catalogFaults} listError={catalogError}
      openCount={workspaces.length} savedCount={savedCount} onRefresh={() => void refreshSaved()} onReopen={name => void reopenWorkspace(name)} reopening={reopening} reopenError={reopenError}
      busyReason={appBusy === 'reopeningWorkspace' ? null : commandReason} strip={strip('saved')}/>
    <SettingsDialog open={dialogOpen} onCancel={() => setDialogOpen(false)} settings={settings} draft={settingsDraft} onDraft={next => {setSettingsDraft(next); setSaveNotice(null);}}
      parsed={parsedSettings} dirty={dialogDirty} saving={appBusy === 'savingSettings'} busyReason={commandReason} saveError={dialogError?.kind === 'save' ? dialogError.value : null} saveNotice={renderMessage(locale, saveNotice)} onSave={() => void saveSettings()}
      envDirty={envDirty} active={active} target={checkTarget} onCheck={() => void checkEnvironment()} lastCheck={lastCheck} stale={checkStale} originLabel={labelOf}
      checkError={dialogError?.kind === 'check' ? dialogError.value : null}
      retained={logs.items.length} evicted={logs.evicted}
      onSnapshot={() => void snapshot(null)} snapshotPending={bootstrap.snapshotPending} snapshotOutcome={bootstrap.snapshotOutcome} strip={strip('dialog')}/>
  </div></LocaleContext>;
}

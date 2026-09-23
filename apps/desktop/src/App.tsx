import {useEffect, useMemo, useRef, useState} from 'react';
import type {KeyboardEvent, ReactNode} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {getCurrentWindow} from '@tauri-apps/api/window';
import {LocalFault, messages, renderMessage} from './i18n.ts';
import type {Command, Locale, Message} from './i18n.ts';
import {LocaleContext, useLocale} from './locale.tsx';
import type {CheckTarget, LastCheck} from './settings/EnvironmentPanel.tsx';
import LogsPage from './pages/LogsPage.tsx';
import type {Losses} from './pages/LogsPage.tsx';
import Notifications from './components/Notifications.tsx';
import ResultPanel, {FaultMessage, fault} from './components/ResultPanel.tsx';
import RunPage from './pages/RunPage.tsx';
import type {Derived, RunHandlers, RunSnapshot, RunView} from './pages/RunPage.tsx';
import SettingsDialog from './settings/SettingsDialog.tsx';
import WorkspaceSwitcher from './components/WorkspaceSwitcher.tsx';
import type {WorkspaceOption} from './components/WorkspaceSwitcher.tsx';
import {dismissCard, emptyStack, ingestCards, interactCard, tickCards, trimCards} from './notifications.ts';
import type {Card, CardStack} from './notifications.ts';
import {DEFAULT_NOTIFICATIONS, acceptController, faultSummary, readDraft, readEnvironment, readSettingsDraft, retainLogs, retainedCheck, sameEnvironment, sameNotifications, settingsDraftAfterSave, settingsDraftFrom, staleReasons, text} from './state.ts';
import type {CheckAssociation, LogStore, SettingsDraft} from './state.ts';
import {DESCRIPTOR_LIMIT, WORKSPACE_LIMIT, applyCommand, applyIfCurrent, busy, closeWorkspace, freshWorkspace, ingestResults, needsAttention, newDraft, openWorkspace, originLabel, retainClosed, selectProfile, updateWorkspace, workspaceLabel, workspaceRef} from './workspace.ts';
import type {ClosedWorkspace, LogFilter, LogScope, Origin, ProfileCatalog, RetainedResult, Workspace, WorkspaceCommand} from './workspace.ts';
import type {ControllerView, Fault, Json, OcrEnvironment, Poll, Profile, Selection, Settings, StartRequest, WorkspaceRef} from './types.ts';

const idle: ControllerView = {run: null, state: 'idle', operation: 'run', result: null, error: null, progress: [], dropped_logs: 0, workspace_id: null, workspace_revision: null};
const EMPTY_FILTER: LogFilter = {text: '', level: ''};
const encoder = new TextEncoder();

type Nav = {kind: 'none'} | {kind: 'workspace'; id: string} | {kind: 'application'} | {kind: 'closed'; id: string};
interface Operation {run: string; kind: 'run' | 'check'; workspace: WorkspaceRef | null; snapshot: RunSnapshot}
interface Starting {workspaceId: string | null; kind: 'run' | 'check'}
interface DialogError {kind: 'save' | 'check'; value: Fault}


function derive(workspace: Workspace, savedEnvironment: OcrEnvironment | null, locale: Locale): Derived {
  const t = messages[locale].app;
  const parsed = readDraft(workspace.package.schema, workspace.draft, locale);
  const numericErrors = Object.keys(parsed.errors).length > 0;
  const selectedProfile = workspace.profiles.find(profile => profile.id === workspace.selectedId);
  const valuesDirty = !selectedProfile || numericErrors || JSON.stringify(parsed.values) !== JSON.stringify(selectedProfile.values);
  const dirty = valuesDirty || workspace.name !== selectedProfile?.name;
  const bound = !selectedProfile || (selectedProfile.package_id === workspace.package.package_id && selectedProfile.schema_identity === workspace.package.schema_identity);
  const descriptor = workspace.descriptorPath.trim();
  const startBlock = workspace.lane === 'replay' && !descriptor ? t.replayDescriptor
    : workspace.lane === 'replay' && !savedEnvironment ? t.replayEnvironment : null;
  const descriptorError = encoder.encode(descriptor).length > DESCRIPTOR_LIMIT ? t.descriptorLimit(DESCRIPTOR_LIMIT) : null;
  return {parsed, numericErrors, valuesDirty, dirty, bound, selectedProfile, startBlock, descriptorError};
}

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
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [closed, setClosed] = useState<ClosedWorkspace[]>([]);
  const [results, setResults] = useState<Record<string, RetainedResult>>({});
  const [nav, setNav] = useState<Nav>({kind: 'none'});
  const [view, setView] = useState<ControllerView>(idle);
  const [operation, setOperation] = useState<Operation | null>(null);
  const [starting, setStarting] = useState<Starting | null>(null);
  const [stopping, setStopping] = useState(false);
  const [stripMessage, setStripMessage] = useState<{text: Message; error: boolean} | null>(null);
  const [closing, setClosing] = useState(false);
  const [settings, setSettings] = useState<Settings | null>(null);
  const locale = settings?.locale ?? 'en';
  const t = messages[locale].app;
  const ui = messages[locale].ui;
  const [logs, setLogs] = useState<LogStore>({items: [], evicted: 0});
  const [losses, setLosses] = useState<Losses>({gui_dropped: 0, file_dropped: 0, file_errors: 0, last_file_error: null});
  const [pollError, setPollError] = useState<Fault | null>(null);
  const [lastCheck, setLastCheck] = useState<LastCheck | null>(null);
  const [cards, setCards] = useState<CardStack>(emptyStack);
  const [appBusy, setAppBusy] = useState<Command | null>('loadingSettings');
  const [appError, setAppError] = useState<Fault | null>(null);
  const [openPath, setOpenPath] = useState('');
  const [openForm, setOpenForm] = useState(false);
  const [pendingClose, setPendingClose] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [settingsDraft, setSettingsDraft] = useState<SettingsDraft>(() => settingsDraftFrom(null));
  const [dialogError, setDialogError] = useState<DialogError | null>(null);
  const [saveNotice, setSaveNotice] = useState<Message | null>(null);
  const [appFilter, setAppFilter] = useState<LogFilter>(EMPTY_FILTER);
  const [appScope, setAppScope] = useState<LogScope>({kind: 'application'});
  const [closedFilter, setClosedFilter] = useState<LogFilter>(EMPTY_FILTER);
  const [reveal, setReveal] = useState<{scope: string; sequence: number} | null>(null);
  const [closedDisclosed, setClosedDisclosed] = useState(false);
  const expectedRun = useRef<string | null>(null);
  const epoch = useRef(0);
  const startInFlight = useRef(false);
  // Mirror host admission synchronously so rapid commands cannot cross workspace attribution.
  const hostCommand = useRef<Command | null>(null);
  const retention = useRef(1000);
  const preferences = useRef(DEFAULT_NOTIFICATIONS);
  const openWorkspaces = useRef(workspaces);
  openWorkspaces.current = workspaces;
  const menuButton = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  const savedEnvironment = settings?.ocr_environment ?? null;
  const derived = useMemo(() => Object.fromEntries(workspaces.map(workspace => [workspace.id, derive(workspace, savedEnvironment, locale)])) as Record<string, Derived>, [workspaces, savedEnvironment, locale]);
  const active = starting !== null || busy(view.state);
  const selected = nav.kind === 'workspace' ? workspaces.find(workspace => workspace.id === nav.id) : undefined;
  const closedSelected = nav.kind === 'closed' ? closed.find(item => item.id === nav.id) : undefined;
  const noSelection = nav.kind === 'none' || (nav.kind === 'workspace' && !selected);
  const labelOf = (workspaceId: string | null) => originLabel(workspaceId, workspaces, closed, locale).label;
  // Settings Save has a separate gate; other pending commands expose their shared refusal reason.
  const busyWorkspace = workspaces.find(workspace => workspace.busy !== null);
  const commandReason = appBusy !== null && appBusy !== 'savingSettings' ? t[appBusy]
    : busyWorkspace ? `${renderMessage(locale, busyWorkspace.busy)} · ${workspaceLabel(busyWorkspace, workspaces)}`
    : starting ? `${starting.kind === 'check' ? t.admittingCheck : t.admittingRun} · ${labelOf(starting.workspaceId)}`
    : null;
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

  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    async function tick() {
      const pollEpoch = epoch.current;
      try {
        const incoming = await invoke<Poll>('poll');
        if (!alive) return;
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
        if (check) setLastCheck(old => old && old.view.run === check.controller.run && old.view.state === check.controller.state ? old : retainedCheck(check));
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
        retention.current = saved.gui_log_limit;
        preferences.current = saved.notifications;
        setLogs(old => retainLogs(old, [], saved.gui_log_limit));
        if (saved.package_path) {
          setOpenPath(saved.package_path);
          setAppBusy('restoringPackage');
          await openPackage(saved.package_path);
        }
      } catch (cause) {
        if (alive) setAppError(fault(cause));
      } finally {
        if (alive) setAppBusy(null);
      }
    }
    void restore();
    void tick();
    return () => {alive = false; window.clearTimeout(timer);};
  }, []);

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

  // Keep command admission through the follow-up listing; only a fresh read is shared with sibling roots.
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
    setWorkspaces(list => {
      return updateWorkspace(applyCommand(list, origin, command), origin.id, workspace => ({...workspace, busy: null}));
    });
  }

  async function readProfiles(workspace: WorkspaceRef): Promise<ProfileCatalog> {
    try {
      return await invoke<ProfileCatalog>('profiles', {workspace});
    } catch (cause) {
      // The mutation already succeeded. Preserve that result without treating a failed read as an empty catalog.
      return {profiles: [], profiles_error: fault(cause)};
    }
  }

  function valuesForCommand(workspace: Workspace): Record<string, Json> {
    const facts = derived[workspace.id];
    if (!facts.bound) throw new LocalFault({key: 'profileBinding'});
    if (facts.numericErrors) throw new LocalFault({key: 'numericFields'});
    return facts.parsed.values;
  }

  async function openPackage(path: string) {
    setAppError(null);
    hostCommand.current = 'inspectingPackage';
    try {
      const selection = await invoke<Selection>('inspect', {packagePath: path, workspace: null});
      setWorkspaces(list => openWorkspace(list, selection));
      go({kind: 'workspace', id: selection.workspace_id});
      setOpenForm(false);
      setOpenPath('');
    } catch (cause) {
      setAppError(fault(cause));
    } finally {
      hostCommand.current = null;
    }
  }

  async function openFromForm() {
    if (hostCommand.current !== null || appBusy || !openPath.trim()) return;
    setAppBusy('inspectingPackage');
    try {await openPackage(openPath);} finally {setAppBusy(null);}
  }

  function handlers(workspace: Workspace): RunHandlers {
    const ref = workspaceRef(workspace);
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const locked = commandReason !== null || closing;
    return {
      change: update => change(workspace.id, update),
      selectProfile: id => change(workspace.id, item => selectProfile(item, id)),
      newDraft: preset => change(workspace.id, item => newDraft(item, preset)),
      validate: () => {
        if (locked) return;
        const checked = workspace.draftRevision;
        void runCommand(origin, 'validatingDraft', async () => {
          const values = valuesForCommand(workspace);
          const effective = await invoke<Record<string, Json>>('validate', {workspace: ref, values});
          return {update: item => item.draftRevision === checked
            ? {...item, validation: effective, notice: {key: 'validated'}}
            : {...item, notice: {key: 'earlierValidated'}}};
        });
      },
      saveProfile: () => {
        if (locked) return;
        const checked = workspace.draftRevision;
        void runCommand(origin, 'savingProfile', async () => {
          const values = valuesForCommand(workspace);
          const profile = await invoke<Profile>('save_profile', {workspace: ref, id: workspace.selectedId, name: workspace.name, values});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({
            ...item, profiles: [...item.profiles.filter(entry => entry.id !== profile.id), profile].sort((a, b) => a.name.localeCompare(b.name)),
            selectedId: profile.id, preset: '',
            ...(item.draftRevision === checked ? {draft: structuredClone(profile.values), validation: null, touched: false} : {}),
            notice: {key: 'profileSaved', args: [profile.name, profile.id]},
          })};
        });
      },
      renameProfile: () => {
        if (locked || !workspace.selectedId) return;
        const id = workspace.selectedId;
        void runCommand(origin, 'renamingProfile', async () => {
          const profile = await invoke<Profile>('rename_profile', {workspace: ref, id, name: workspace.name});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...item, profiles: item.profiles.map(entry => entry.id === profile.id ? profile : entry), notice: {key: 'profileRenamed', args: [profile.name]}})};
        });
      },
      deleteProfile: () => {
        if (locked || !workspace.selectedId) return;
        const id = workspace.selectedId;
        void runCommand(origin, 'deletingProfile', async () => {
          await invoke('delete_profile', {workspace: ref, id});
          const catalog = await readProfiles(ref);
          return {catalog, update: item => ({...item, profiles: item.profiles.filter(entry => entry.id !== id), selectedId: item.selectedId === id ? null : item.selectedId, touched: true,
            notice: {key: 'profileDeleted'}})};
        });
      },
      reinspect: () => {
        const current = runView(workspace);
        if (locked || (current.live && busy(current.view.state))) return;
        void runCommand(origin, 'reinspectingPackage', async () => {
          const selection = await invoke<Selection>('inspect', {packagePath: workspace.packagePath, workspace: ref});
          return {catalog: selection, update: item => ({...freshWorkspace(selection, item), notice: {key: 'reinspected'}})};
        });
      },
      start: () => void startRun(workspace),
      stop: () => void stopRun(),
    };
  }

  async function startRun(workspace: Workspace) {
    const facts = derived[workspace.id];
    if (hostCommand.current !== null || active || closing || facts.startBlock || facts.descriptorError) return;
    let values: Record<string, Json>;
    try {values = valuesForCommand(workspace);} catch (cause) {const error = fault(cause); change(workspace.id, item => ({...item, error})); return;}
    const profile = facts.selectedProfile;
    const profileId = profile && !facts.valuesDirty ? profile.id : 'draft';
    const replay = workspace.lane === 'replay';
    const request: StartRequest = {
      package_path: workspace.packagePath, inventory_identity: workspace.package.inventory_identity,
      package_id: profile?.package_id ?? workspace.package.package_id, schema_identity: profile?.schema_identity ?? workspace.package.schema_identity,
      profile_id: profileId, values, lane: workspace.lane, scenario: replay ? 'workflow' : workspace.scenario,
      replay_descriptor_path: replay ? workspace.descriptorPath.trim() || null : null,
    };
    const profileName = profile && !facts.valuesDirty ? profile.name : workspace.name;
    const ref = workspaceRef(workspace);
    hostCommand.current = 'admittingRun';
    startInFlight.current = true;
    epoch.current += 1;
    setStarting({workspaceId: workspace.id, kind: 'run'});
    setStripMessage(null);
    change(workspace.id, item => ({...item, error: null, notice: null, disclosedRun: null}));
    try {
      const run = await invoke<string>('start', {workspace: ref, request});
      expectedRun.current = run;
      setOperation({run, kind: 'run', workspace: ref, snapshot: {kind: 'run', run, lane: workspace.lane, packageId: request.package_id, profileName, profileId, scenario: request.scenario, descriptorPath: request.replay_descriptor_path, values}});
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
  const dialogDirty = settings === null || settingsDraft.locale !== settings.locale || settingsDraft.logLimit.trim() !== String(settings.gui_log_limit) || !sameNotifications(settingsDraft.notifications, settings.notifications) || envDirty;
  // Check binds the workspace visible when the dialog opened; the modal prevents that selection from changing.
  const checkTarget: CheckTarget = selected
    ? {workspace: workspaceRef(selected), label: workspaceLabel(selected, workspaces), descriptorPath: selected.descriptorPath.trim() || null, packageInventoryIdentity: selected.package.inventory_identity}
    : {workspace: null, label: t.none, descriptorPath: null, packageInventoryIdentity: null};
  const checkStale = lastCheck ? staleReasons(lastCheck.association, {
    saved: savedEnvironment, draftDirty: envDirty, workspace: checkTarget.workspace, descriptorPath: checkTarget.descriptorPath, packageInventoryIdentity: checkTarget.packageInventoryIdentity,
  }, locale) : [];

  // Check reads saved settings only; the association records exactly what the backend will read.
  async function checkEnvironment() {
    if (hostCommand.current !== null || active || closing || envDirty || appBusy) return;
    if (selected && derived[selected.id].descriptorError) {
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
    if (settings === null) return;
    menuButton.current?.focus();
    setMenuOpen(false);
    setSettingsDraft(settingsDraftFrom(settings));
    setDialogError(null);
    setSaveNotice(null);
    setDialogOpen(true);
  }

  async function saveSettings() {
    const editable = parsedSettings.settings;
    if (settings === null || !editable || appBusy || commandReason !== null) return;
    const submitted = settingsDraft;
    setAppBusy('savingSettings');
    setDialogError(null);
    try {
      const saved = await invoke<Settings>('save_settings', {settings: editable});
      setSettings(saved);
      retention.current = saved.gui_log_limit;
      preferences.current = saved.notifications;
      setLogs(old => retainLogs(old, [], saved.gui_log_limit));
      setCards(old => trimCards(old, saved.notifications.visible_count));
      setSettingsDraft(current => settingsDraftAfterSave(current, submitted, saved));
      setSaveNotice(saved.ocr_environment ? {key: 'settingsSaved', args: [saved.ocr_environment.profile]} : {key: 'settingsSavedEmpty'});
    } catch (cause) {
      setDialogError({kind: 'save', value: fault(cause)});
    } finally {
      setAppBusy(null);
    }
  }

  function focusWorkspaceSelection() {
    requestAnimationFrame(() => {
      const selector = document.getElementById('workspace-select') as HTMLButtonElement | null;
      (selector && !selector.disabled ? selector : document.getElementById('package-path'))?.focus();
    });
  }

  async function closeTab(workspace: Workspace, confirmed: boolean) {
    const current = runView(workspace);
    if (hostCommand.current !== null) {const pending = hostCommand.current; change(workspace.id, item => ({...item, notice: {key: 'waitForCommand', args: [pending]}})); return;}
    if (current.live && busy(current.view.state)) {change(workspace.id, item => ({...item, notice: {key: 'ownerCannotClose'}})); return;}
    if (derived[workspace.id].dirty && workspace.touched && !confirmed) {setPendingClose(workspace.id); return;}
    setPendingClose(null);
    hostCommand.current = 'closingWorkspace';
    change(workspace.id, item => ({...item, busy: {key: 'closingWorkspace'}, error: null}));
    try {
      await invoke('close_workspace', {workspace: workspaceRef(workspace)});
      const index = workspaces.findIndex(item => item.id === workspace.id);
      const neighbor = workspaces[index + 1] ?? workspaces[index - 1];
      setClosed(old => retainClosed(old, {id: workspace.id, revision: workspace.revision, label: workspaceLabel(workspace, workspaces), result: current.view.run ? current.view : null}));
      setWorkspaces(list => closeWorkspace(list, workspace.id));
      setResults(old => {
        if (!(workspace.id in old)) return old;
        const next = {...old};
        delete next[workspace.id];
        return next;
      });
      if (nav.kind === 'workspace' && nav.id === workspace.id) go(neighbor ? {kind: 'workspace', id: neighbor.id} : {kind: 'none'});
    } catch (cause) {
      const error = fault(cause);
      change(workspace.id, item => ({...item, busy: null, error}));
    } finally {
      hostCommand.current = null;
      focusWorkspaceSelection();
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
  const workspaceItems: WorkspaceOption[] = workspaces.map(workspace => {
    const current = runView(workspace);
    const facts = derived[workspace.id];
    const owns = starting?.workspaceId === workspace.id || (current.live && busy(current.view.state));
    const issue = workspace.error?.category ?? workspace.profilesError?.category
      ?? (facts.numericErrors ? t.invalidFields : !facts.bound ? t.staleProfile : facts.descriptorError ? t.invalidDescriptor : null);
    const attention = issue !== null || needsAttention(current.view);
    const status: WorkspaceOption['status'] = owns ? {kind: 'busy', text: `${ui.phase(starting?.workspaceId === workspace.id ? 'preparing' : current.view.state)} · ${ui.operation(starting?.workspaceId === workspace.id ? starting.kind === 'check' ? 'environment_check' : 'run' : current.view.operation)}`}
      : workspace.busy ? {kind: 'busy', text: renderMessage(locale, workspace.busy)}
      : attention ? {kind: 'attention', text: t.attention(issue ?? current.view.error?.category ?? text(current.view.result?.status) ?? t.unresolved)}
      : facts.dirty && workspace.touched ? {kind: 'dirty', text: t.unsavedDraft}
      : {kind: 'ready', text: t.ready(facts.selectedProfile ? facts.selectedProfile.name : t.draft)};
    return {id: workspace.id, path: workspace.packagePath, label: workspaceLabel(workspace, workspaces), status, running: owns, attention};
  });
  const openDisabled = commandReason !== null || closing;
  const openFormView = <section className="panel open-form" aria-labelledby="open-heading">
    <div className="panel-body">
      <h2 id="open-heading">{t.openHeading}</h2>
      <p className="muted">{t.openHelp(WORKSPACE_LIMIT)}</p>
      <div className="open-row"><div className="field"><label htmlFor="package-path">{t.packageDirectory}</label>
        <input id="package-path" type="text" value={openPath} disabled={appBusy !== null || closing} placeholder={t.packagePlaceholder} spellCheck={false}
          onChange={event => {setOpenPath(event.target.value); setAppError(null);}} onKeyDown={event => {if (event.key === 'Enter') void openFromForm();}}/></div>
        <button id="inspect" className="primary" disabled={openDisabled || !openPath.trim()} onClick={() => void openFromForm()}>{t.inspect}</button>
        {workspaces.length > 0 && <button type="button" onClick={() => {setOpenForm(false); setAppError(null);}}>{t.cancel}</button>}</div>
      <div className="operation-status" role="status">{commandReason ?? (workspaces.length >= WORKSPACE_LIMIT ? t.workspaceLimit(WORKSPACE_LIMIT) : '')}</div>
      {appError && <><FaultMessage title={t.openFailed} value={appError}/><p className="muted">{t.openRecovery}</p></>}
    </div>
  </section>;

  return <LocaleContext value={locale}><div className="app">
    <header className="topbar">
      <div className="brand"><span className="brandmark" aria-hidden="true">M</span><span>MadoMata</span><span className="divider" aria-hidden="true"/><span className="eyebrow">{t.workspace}</span></div>
      <div className="topbar-actions">
        <span className="lane-badge">{t.controlledScope}</span>
        <div className="menu-anchor">
          <button id="application-menu" ref={menuButton} type="button" aria-haspopup="menu" aria-expanded={menuOpen} aria-controls="application-menu-items" onClick={() => setMenuOpen(open => !open)}>{t.application} ▾</button>
          {menuOpen && <div id="application-menu-items" ref={menu} className="dropdown" role="menu" aria-labelledby="application-menu" onKeyDown={menuKeys}>
            <button type="button" role="menuitem" id="menu-settings" disabled={settings === null} onClick={openSettings}>{t.appSettings}</button>
            <button type="button" role="menuitem" id="menu-application-logs" aria-current={nav.kind === 'application' ? 'page' : undefined} onClick={() => {setMenuOpen(false); go({kind: 'application'});}}>{t.applicationLogs}<span className="count">{logCounts[''] ?? 0}</span></button>
            <button type="button" role="menuitem" id="close" disabled={closing} onClick={async () => {
              setMenuOpen(false);
              setClosing(true);
              try {await getCurrentWindow().close();} catch (cause) {setAppError(fault(cause)); setClosing(false);}
            }}>{closing ? t.closing : t.closeWindow}</button>
            <p className="menu-description">{t.menuDescription}</p>
          </div>}
        </div>
      </div>
    </header>
    <div className="workspace-bar">
      <div className="workspace-switcher">
        <WorkspaceSwitcher items={workspaceItems} selectedId={selected?.id ?? null} onSelect={id => go({kind: 'workspace', id})}
          onClose={selected ? () => void closeTab(selected, false) : null}
          closeReason={active && owner === selected?.id ? t.ownsOperation : commandReason ?? (closing ? t.applicationClosing : null)}/>
        {workspaces.length > 0 && <button id="open-workspace" type="button" className="workspace-action" aria-label={t.openAnother} aria-expanded={openForm} disabled={appBusy !== null || closing}
          title={workspaces.length >= WORKSPACE_LIMIT ? t.activateOrClose : t.openAnother} onClick={() => {setOpenForm(open => !open); setAppError(null);}}>+</button>}
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
      <button type="button" autoFocus onClick={() => {setPendingClose(null); focusWorkspaceSelection();}}>{t.keepOpen}</button>
    </div>}
    {strip('app')}
    {pollError && <div className="content-wide"><FaultMessage title={t.connectionFailed} value={pollError}/></div>}
    {openForm && !noSelection && <div className="content-wide">{openFormView}</div>}
    <div className="workspace">
      <main id="workspace-panel" className="content" aria-label={selected ? t.workspaceAria(workspaceLabel(selected, workspaces)) : undefined}>
        {selected && selected.page === 'run' && <RunPage key={`${selected.id}:${selected.revision}`} workspace={selected} label={workspaceLabel(selected, workspaces)} derived={derived[selected.id]} run={runView(selected)}
          snapshot={operation && operation.workspace?.workspace_id === selected.id ? operation.snapshot : null}
          locked={commandReason !== null || closing} active={active} starting={starting?.workspaceId === selected.id} stopping={stopping} closing={closing}
          savedEnvironment={savedEnvironment} handlers={handlers(selected)}/>}
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
            <p>{workspaces.length === 0 ? t.firstWorkspace : t.chooseHelp}</p></div></div>
          {openFormView}
        </>}
      </main>
    </div>
    <Notifications cards={cards.cards} scopeLabel={labelOf} onDismiss={id => setCards(old => dismissCard(old, id))} onOpen={openDiagnostics}
      onInteract={(id, interaction) => setCards(old => interactCard(old, id, interaction))}/>
    <SettingsDialog open={dialogOpen} onCancel={() => setDialogOpen(false)} settings={settings} draft={settingsDraft} onDraft={next => {setSettingsDraft(next); setSaveNotice(null);}}
      parsed={parsedSettings} dirty={dialogDirty} saving={appBusy === 'savingSettings'} busyReason={commandReason} saveError={dialogError?.kind === 'save' ? dialogError.value : null} saveNotice={renderMessage(locale, saveNotice)} onSave={() => void saveSettings()}
      envDirty={envDirty} active={active} target={checkTarget} onCheck={() => void checkEnvironment()} lastCheck={lastCheck} stale={checkStale} originLabel={labelOf}
      checkError={dialogError?.kind === 'check' ? dialogError.value : null}
      retained={logs.items.length} evicted={logs.evicted} strip={strip('dialog')}/>
  </div></LocaleContext>;
}

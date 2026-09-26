import {useEffect, useMemo, useReducer, useRef, useState} from 'react';
import type {KeyboardEvent, ReactNode} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {emitTo, listen} from '@tauri-apps/api/event';
import {getCurrentWindow} from '@tauri-apps/api/window';
import {LocalFault, messages, renderMessage} from './i18n.ts';
import type {Command, Message} from './i18n.ts';
import {LocaleContext, useLocale} from './locale.tsx';
import type {CheckTarget, LastCheck} from './settings/EnvironmentPanel.tsx';
import BootstrapPage from './pages/BootstrapPage.tsx';
import EditPage from './pages/EditPage.tsx';
import type {EditHandlers, PageAuthoring} from './pages/EditPage.tsx';
import RecognitionPage from './pages/RecognitionPage.tsx';
import * as recognition from './recognition.ts';
import GuidancePage from './pages/GuidancePage.tsx';
import type {ActiveOwner} from './pages/GuidancePage.tsx';
import LogsPage from './pages/LogsPage.tsx';
import type {Losses} from './pages/LogsPage.tsx';
import CreateWorkspaceDialog from './components/CreateWorkspaceDialog.tsx';
import type {CreateDraft} from './components/CreateWorkspaceDialog.tsx';
import DirtyChoiceDialog from './components/DirtyChoiceDialog.tsx';
import Notifications from './components/Notifications.tsx';
import type {RecoveryHandlers} from './components/ProfileRecovery.tsx';
import ResultPanel, {FaultMessage, fault} from './components/ResultPanel.tsx';
import RunPage from './pages/RunPage.tsx';
import type {RunHandlers, RunSnapshot, RunView} from './pages/RunPage.tsx';
import SavedWorkspacesDialog from './components/SavedWorkspacesDialog.tsx';
import Select from './components/Select.tsx';
import SettingsDialog from './settings/SettingsDialog.tsx';
import WorkspaceSwitcher from './components/WorkspaceSwitcher.tsx';
import type {WorkspaceOption} from './components/WorkspaceSwitcher.tsx';
import {INITIAL_BOOTSTRAP, PollGate, initialSettings, reconstructed, reduceBootstrap, surface} from './bootstrap.ts';
import type {Admission, BootstrapAction} from './bootstrap.ts';
import {dismissCard, emptyStack, ingestCards, interactCard, tickCards, trimCards} from './notifications.ts';
import type {Card, CardStack} from './notifications.ts';
import {DEFAULT_NOTIFICATIONS, acceptController, faultSummary, readEnvironment, readSettingsDraft, retainLogs, retainedCheck, sameEnvironment, sameNotifications, settingsDraftAfterSave, settingsDraftFrom, staleReasons, text} from './state.ts';
import type {CheckAssociation, LogStore, SettingsDraft} from './state.ts';
import {editRecovery, readRecoveryDraft, recoveryTicket, selectRecovery} from './recovery.ts';
import type {RecoveryState, RecoveryTicket} from './recovery.ts';
import {applyCatalogMutation, applyRecognitionMutation, applyRefresh, applySave, applyValidation, beginComposition, beginPending, catalogTicket, dirtyDrafts, discardFile, editFile, endComposition, failCommand, openSession, recordRange, redoFile, replaceFile, revealRange, sameAuthoringRef, saveBlock, saveTicket, selectFile, selectRecognition, undoFile, validationTicket} from './authoring.ts';
import type {AuthoringSession} from './authoring.ts';
import {DESCRIPTOR_LIMIT, UNSUPPORTED_SOURCE, WORKSPACE_LIMIT, applyAuthoringExit, applyCommand, applyIfCurrent, applyInspection, applyInvalidatedViews, applyRecoveryMutation, applyWorkspaceView, busy, closeWorkspace, commandValues, deriveBound, hasWorkspaceEdits, ingestResults, isBound, needsAttention, newDraft, originLabel, retainClosed, selectProfile, updateBound, updateWorkspace, workspaceFromView, workspaceLabel, workspaceRef} from './workspace.ts';
import type {Bound, BoundWorkspace, ClosedWorkspace, Derived, LogFilter, LogScope, Origin, RetainedResult, Workspace, WorkspaceCommand} from './workspace.ts';
import {beginApplicationPicker, beginRunningApplication, beginTarget, cancelRunningApplication, checkedTarget, completeApplicationPicker, completeRunningApplication, currentTargetDraft, discardTarget, editTarget, eligibleRunningApplication, failRunningApplication, invalidateApplicationPicker, invalidateRunningApplication, readTarget, readTargetDraft, removedTarget, savedTarget, targetFailed, targetReadFailed, targetTicket} from './target.ts';
import type {TargetOperation, TargetPickerField, TargetPickerTicket, TargetState} from './target.ts';
import type {AuthoringMutation, AuthoringRef, AuthoringValidation, AuthoringView, BootstrapStatus, CatalogEdit, ControllerView, Fault, InspectionOutcome, Json, LegacyImport, Poll, Profile, ProfileCatalog, RecoveryMutation, Settings, SnapshotReceipt, StartRequest, TabRecord, TargetApplicationResponse, TargetCheckResponse, TargetResolution, TargetSaveResponse, TargetView, WorkspaceCatalog, WorkspaceRef, WorkspaceView} from './types.ts';

const idle: ControllerView = {run: null, state: 'idle', operation: 'run', result: null, error: null, progress: [], dropped_logs: 0, workspace_id: null, workspace_revision: null};
const EMPTY_FILTER: LogFilter = {text: '', level: ''};
const EMPTY_CREATE: CreateDraft = {internalName: '', displayName: ''};

type Nav = {kind: 'none'} | {kind: 'workspace'; id: string} | {kind: 'application'} | {kind: 'closed'; id: string};
interface Operation {run: string; kind: 'run' | 'check'; workspace: WorkspaceRef | null; snapshot: RunSnapshot}
interface Starting {workspaceId: string | null; kind: 'run' | 'check'}
interface DialogError {kind: 'save' | 'check'; value: Fault}
// An action that ends or replaces the Edit session; unsaved drafts are resolved by Save/Discard/Cancel first.
type Choice = {kind: 'exit'} | {kind: 'duplicate'; packageId: string} | {kind: 'close'} | {kind: 'closeTab'; workspaceId: string};
// The host's category for a Workspace reference it no longer recognizes (closed, reinspected or invalidated by Edit).
const STALE_IDENTITY = 'StaleIdentity';

// The notice for a typed inspection result. A binding failure is the Tab's error, not a notice; a recovery-required
// candidate and automatic per-profile outcomes are named so the operator finds the recovery panel.
function inspectionNotice(outcome: InspectionOutcome, rebinding: boolean, retry: boolean): Message | null {
  if (outcome.kind === 'binding_failed') return null;
  if (outcome.kind === 'recovery_required') return {key: 'recoveryRequired'};
  if (retry) return {key: 'recoveryBound'};
  if (outcome.outcomes.length > 0) {
    const saved = outcome.outcomes.filter(item => item.status === 'saved').length;
    return {key: 'inspectionOutcomes', args: [saved, outcome.outcomes.length - saved]};
  }
  return {key: rebinding ? 'reinspected' : 'bound'};
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

// The application-wide Edit lease stays visible with Return to Edit wherever the editor itself is not shown.
function AuthoringStrip({idPrefix, owner, packageId, unsaved, showReturn, onReturn}: {
  idPrefix: string; owner: string; packageId: string | null; unsaved: number; showReturn: boolean; onReturn: () => void;
}) {
  const locale = useLocale();
  const a = messages[locale].ui.authoring;
  return <div id={`${idPrefix}-authoring-strip`} className="operation-strip authoring-strip" role="status" aria-live="polite">
    <span className="strip-owner"><span className="eyebrow">{a.ownerEyebrow}</span><strong>{owner}</strong></span>
    <span className="strip-kind">{packageId === null ? a.stripUnknown : a.stripKind(packageId)}</span>
    {unsaved > 0 && <span className="tag unsaved">{a.unsavedFiles(unsaved)}</span>}
    <span className="strip-note">{a.stripNote}</span>
    {showReturn && <button id={`${idPrefix}-return-to-edit`} type="button" onClick={onReturn}>{a.returnToEdit}</button>}
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
  const [pickerBusy, setPickerBusy] = useState(false);
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
  // The one Edit session's drafts live here, not in the page, so navigation and unmounting never lose them. The ref is
  // the synchronous truth for sequential host calls (Save all) and is published to state by `updateAuthoring` only.
  const authoringStore = useRef<AuthoringSession | null>(null);
  const [authoring, setAuthoring] = useState<AuthoringSession | null>(null);
  // The host's lease as last polled; undefined until the first poll of this session answers.
  const hostAuthoringRef = useRef<AuthoringRef | null | undefined>(undefined);
  const [hostAuthoring, setHostAuthoring] = useState<AuthoringRef | null | undefined>(undefined);
  // Bumped around every Edit host command, so a poll answered before its reply cannot overwrite the lease it created.
  const authoringEpoch = useRef(0);
  const [authoringBusy, setAuthoringBusy] = useState<Command | null>(null);
  const [choice, setChoice] = useState<Choice | null>(null);
  const [choiceBusy, setChoiceBusy] = useState(false);
  const choiceRunning = useRef(false);
  const authoringWorker = useRef<Promise<unknown> | null>(null);
  const capabilityOwner = useRef<string | null>(null);
  const previewConnected = useRef(false);
  const previewBridge = useRef({ready: () => {}, edit: (_message: recognition.PreviewEditMessage) => {}});
  const knownOcrEnvironment = useRef<Settings['ocr_environment']>(null);
  const recognitionConfigurationDirty = useRef(false);
  const expectedRun = useRef<string | null>(null);
  const epoch = useRef(0);
  const sessionGeneration = useRef(0);
  const startInFlight = useRef(false);
  // Mirror host admission synchronously so rapid commands cannot cross workspace attribution.
  const hostCommand = useRef<Command | null>(null);
  const applicationRequest = useRef<{workspace:WorkspaceRef; requestId:string}|null>(null);
  const pickerRequest = useRef<{picker:TargetPickerTicket}|null>(null);
  const bootstrapBusy = useRef(false);
  // Set when a constructing action ended Ready before its catalog could be read; the next catalog decides the rebuild.
  const pendingReconstruction = useRef(false);
  // Serializes polls against reconstruction: a constructing action holds it before its host call (see PollGate).
  const [pollGate] = useState(() => new PollGate());
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
  const defaultPackagesRoot = status?.default_packages_root ?? '';
  const packagesRoot = settings?.packages_root ?? defaultPackagesRoot;
  const derived = useMemo(() => Object.fromEntries(workspaces.filter(isBound).map(workspace => [workspace.id, deriveBound(workspace.bound, savedEnvironment, locale)])) as Record<string, Derived>, [workspaces, savedEnvironment, locale]);
  const authoringWorkerPending = authoring?.pending?.kind === 'recognition_trial' || authoring?.pending?.kind === 'validate';
  const active = starting !== null || busy(view.state) || authoringWorkerPending;
  const selected = nav.kind === 'workspace' ? workspaces.find(workspace => workspace.id === nav.id) : undefined;
  const closedSelected = nav.kind === 'closed' ? closed.find(item => item.id === nav.id) : undefined;
  const noSelection = nav.kind === 'none' || (nav.kind === 'workspace' && !selected);
  const labelOf = (workspaceId: string | null) => originLabel(workspaceId, workspaces, closed, locale).label;
  // Profile and machine-local target edits share the same explicit discard boundary.
  const dirtyDraft = (workspace: Workspace) => hasWorkspaceEdits(workspace, derived[workspace.id]);
  // Settings Save has a separate gate; other pending commands expose their shared refusal reason.
  const busyWorkspace = workspaces.find(workspace => workspace.busy !== null);
  const pendingCommandReason = appBusy !== null && appBusy !== 'savingSettings' ? t[appBusy]
    : authoringBusy !== null ? t[authoringBusy]
    : bootstrap.pending !== null ? t[bootstrap.pending]
    : busyWorkspace ? `${renderMessage(locale, busyWorkspace.busy)} · ${workspaceLabel(busyWorkspace, workspaces)}`
    : starting ? `${starting.kind === 'check' ? t.admittingCheck : t.admittingRun} · ${labelOf(starting.workspaceId)}`
    : null;
  const normalReady = status?.state === 'ready' && !pendingReconstruction.current;
  const commandReason = pendingCommandReason ?? (normalReady ? null : ui.bootstrap.applicationUnavailable);
  // Edit owner: the local session's, otherwise the host's lease (for example after the view was reloaded).
  const leaseOwnerId = authoring?.owner.workspace.workspace_id ?? hostAuthoring?.workspace.workspace_id ?? null;
  const leaseLabel = leaseOwnerId === null ? null : labelOf(leaseOwnerId);
  const authoringReason = leaseLabel === null ? null : ui.authoring.startBlocked(leaseLabel);
  // The host stopped reporting this view's lease while no Edit command could explain the difference.
  const leaseLost = authoring !== null && hostAuthoring !== undefined && authoring.pending === null && authoringBusy === null
    && hostAuthoring?.token !== authoring.owner.token;
  const recognitionDirty = authoring?.recognition ? recognition.recognitionDirty(authoring.recognition) : false;
  const unsavedCount = (authoring ? dirtyDrafts(authoring).length : 0) + Number(recognitionDirty);
  const validationActive = busy(view.state) && view.operation === 'authoring_validate';
  const recognitionActive = authoring?.pending?.kind === 'recognition_trial' || (busy(view.state) && view.operation.startsWith('recognition_'));
  // The bootstrap action in flight disables its own buttons; it is not a foreign command for admission text.
  const admission: Admission = {active, authoring: leaseOwnerId !== null, command: (pendingCommandReason !== null && bootstrap.pending === null) || closing, anyDirty: workspaces.some(dirtyDraft) || unsavedCount > 0};
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
  // Authoring workers are reported by Edit and the global strip, never as the Tab's run result.
  function runView(workspace: Workspace): RunView {
    if (view.workspace_id === workspace.id && view.run !== null && view.operation !== 'authoring_validate' && !view.operation.startsWith('recognition_')) {
      return {view, live: true, olderRevision: view.workspace_revision !== null && view.workspace_revision !== workspace.revision ? view.workspace_revision : null};
    }
    const retained = results[workspace.id];
    if (retained && retained.view.operation !== 'authoring_validate' && !retained.view.operation.startsWith('recognition_')) return {view: retained.view, live: false, olderRevision: retained.ref.revision !== workspace.revision ? retained.ref.revision : null};
    return {view: idle, live: false, olderRevision: null};
  }

  function updateAuthoring(update: (session: AuthoringSession | null) => AuthoringSession | null) {
    const next = update(authoringStore.current);
    if (next === authoringStore.current) return;
    authoringStore.current = next;
    setAuthoring(next);
  }

  function publishHostAuthoring(next: AuthoringRef | null | undefined) {
    const current = hostAuthoringRef.current;
    if (next === current || (next && current && sameAuthoringRef(next, current))) return;
    hostAuthoringRef.current = next;
    setHostAuthoring(next);
  }

  // Read from refs so an action that awaited the host sees the lease as it is now, not as it was when it was rendered.
  function leaseOwnerNow(): string | null {
    return authoringStore.current?.owner.workspace.workspace_id ?? hostAuthoringRef.current?.workspace.workspace_id ?? null;
  }

  useEffect(() => {document.documentElement.lang = locale;}, [locale]);

  function adoptSettings(saved: Settings) {
    if (!sameEnvironment(knownOcrEnvironment.current, saved.ocr_environment)) {
      recognitionConfigurationDirty.current = true;
      updateAuthoring(session => session?.recognition
        ? {...session, recognition: recognition.invalidateConfiguration(session.recognition)} : session);
    }
    knownOcrEnvironment.current = saved.ocr_environment;
    setSettings(saved);
    dispatch({type:'presentation', locale:saved.locale});
    retention.current = saved.gui_log_limit;
    preferences.current = saved.notifications;
    setLogs(old => retainLogs(old, [], saved.gui_log_limit));
  }

  function cancelApplicationRequest(ownerId?:string) {
    const pending = applicationRequest.current;
    if (!pending || (ownerId && pending.workspace.workspace_id !== ownerId)) return;
    applicationRequest.current = null;
    void invoke<boolean>('cancel_running_application', {workspace:pending.workspace, requestId:pending.requestId}).catch(() => {});
  }

  function invalidateOwnerTarget(ownerId:string) {
    cancelApplicationRequest(ownerId);
    setWorkspaces(list => updateWorkspace(list, ownerId, item => item.bound
      ? updateBound(item, bound => ({...bound, target:invalidateApplicationPicker(invalidateRunningApplication(bound.target))}))
      : item));
  }
  function invalidateAllTargets() {
    cancelApplicationRequest();
    setWorkspaces(list => list.map(item => item.bound
      ? updateBound(item, bound => ({...bound, target:invalidateApplicationPicker(invalidateRunningApplication(bound.target))}))
      : item));
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
    cancelApplicationRequest();
    sessionGeneration.current += 1;
    epoch.current += 1;
    // A rebuilt Application issues fresh leases; the old session's Edit view and its lease are gone with it.
    authoringEpoch.current += 1;
    updateAuthoring(() => null);
    publishHostAuthoring(undefined);
    setChoice(null);
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
    if (constructing) invalidateAllTargets();
    dispatch({type: 'pending', action});
    // A constructing action may replace the Application. Holding the gate before the host call keeps the poll already
    // in flight inside the old session and dispatches no new poll until the resulting status, and with it the session
    // reset, has been applied; a refusal simply resumes polling on the retained session. Nothing drained is discarded.
    const held = constructing ? pollGate.hold() : null;
    try {
      if (held) await held;
      const next = await call();
      const context = next.fault?.context;
      // A rollback awaiting cleanup needs a Recovery notice but has not consumed the original generation's receipt.
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
      if (held) pollGate.resume();
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
  // A held gate or a reconstruction awaiting its catalog defers the tick instead of polling; the rebuilt Application's
  // first events are then drained only after the session reset has been applied.
  useEffect(() => {
    if (!pollEnabled) return;
    let alive = true;
    let timer: number | undefined;
    async function tick() {
      if (pendingReconstruction.current || !pollGate.claim()) {
        if (alive) timer = window.setTimeout(tick, 150);
        return;
      }
      const pollEpoch = epoch.current;
      const pollSession = sessionGeneration.current;
      const pollAuthoring = authoringEpoch.current;
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
        // An Edit command settled during this poll decides the lease itself; this answer may predate it.
        if (pollAuthoring === authoringEpoch.current) publishHostAuthoring(incoming.authoring ?? null);
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
        pollGate.release();
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
    if (nav.kind === 'workspace' && (next.kind !== 'workspace' || next.id !== nav.id)) invalidateOwnerTarget(nav.id);
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

  // Inspection is scoped to the initiating Tab. A bound outcome publishes only after the host's durable bind; a
  // recovery-required candidate never passes through binding and a failed binding keeps the saved reference.
  function inspectFor(workspace: Workspace) {
    const path = workspace.inspectPath.trim();
    if (!path || commandReason !== null || closing || leaseOwnerNow() === workspace.id) return;
    const current = runView(workspace);
    if (current.live && busy(current.view.state)) return;
    invalidateOwnerTarget(workspace.id);
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const ref = workspaceRef(workspace);
    const rebinding = workspace.bound !== null;
    void runCommand(origin, rebinding ? 'reinspectingPackage' : 'inspectingPackage', async () => {
      const outcome = await invoke<InspectionOutcome>('inspect', {packagePath: path, workspace: ref});
      return {update: item => applyInspection(item, outcome, inspectionNotice(outcome, rebinding, false), false)};
    });
  }

  // Recovery commands carry only the host context reference, profile ID and values; replies are guarded by
  // Workspace/revision (runCommand) and by the ticket's token, profile and draft revision. Only Save reads the draft:
  // Reset discards it, so a draft that cannot parse never blocks the Reset meant to replace it.
  function recoveryHandlers(workspace: Workspace): RecoveryHandlers {
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const locked = commandReason !== null || closing;
    const state = workspace.recovery;
    const edit = (update: (state: RecoveryState) => RecoveryState) =>
      change(workspace.id, item => item.recovery ? {...item, recovery: update(item.recovery), notice: null} : item);
    function mutate(label: Command, call: (ticket: RecoveryTicket) => Promise<RecoveryMutation>) {
      if (locked || !state) return;
      const ticket = recoveryTicket(state);
      if (!ticket) return;
      void runCommand(origin, label, async () => {
        const mutation = await call(ticket);
        return {update: item => applyRecoveryMutation(item, ticket, mutation)};
      });
    }
    return {
      select: id => edit(current => selectRecovery(current, id)),
      edit: (values, change) => edit(current => editRecovery(current, values, change)),
      save: () => {
        if (!state) return;
        const parsed = readRecoveryDraft(state, locale);
        if (Object.keys(parsed.errors).length > 0) {
          const error = new LocalFault({key: 'numericFields'});
          change(workspace.id, item => ({...item, error}));
          return;
        }
        mutate('repairingProfile', ticket => invoke<RecoveryMutation>('repair_profile', {context: ticket.context, id: ticket.profileId, values: parsed.values}));
      },
      // Reset reaches the host only through the confirmed button; Cancel in the panel never gets here.
      reset: () => mutate('resettingProfile', ticket => invoke<RecoveryMutation>('reset_profile', {context: ticket.context, id: ticket.profileId, confirm: true})),
      retry: () => {
        if (locked || !state) return;
        const context = state.view.context;
        void runCommand(origin, 'retryingBinding', async () => {
          const outcome = await invoke<InspectionOutcome>('retry_binding', {context});
          return {update: item => applyInspection(item, outcome, inspectionNotice(outcome, true, true), true)};
        });
      },
      discard: () => {
        if (locked || !state) return;
        const context = state.view.context;
        void runCommand(origin, 'discardingRecovery', async () => {
          const view = await invoke<WorkspaceView>('discard_recovery', {context});
          return {update: item => applyWorkspaceView(item, view, {key: 'recoveryDiscarded'})};
        });
      },
    };
  }

  async function targetCommand(workspace: BoundWorkspace, operation: TargetOperation, reviewedResolution?: TargetResolution) {
    if (hostCommand.current !== null || closing || !normalReady || (operation !== 'read' && active)
      || (operation !== 'read' && pickerRequest.current !== null) || workspace.bound.target.operation !== null) return;
    const target = workspace.bound.target;
    const ticket = targetTicket(target);
    const configuration = readTargetDraft(target.draft).configuration;
    if (operation !== 'read' && (!ticket || (operation !== 'remove' && (!target.declaration || !configuration)))) return;
    if (reviewedResolution && (!target.review || !currentTargetDraft(target, target.review.ticket)
      || JSON.stringify(reviewedResolution) !== JSON.stringify(target.review.resolution))) return;
    cancelApplicationRequest(workspace.id);
    const labels = {read:'readingTarget', check:'checkingTarget', save:'savingTarget', remove:'removingTarget'} as const;
    const label = labels[operation];
    const origin = {id:workspace.id, revision:workspace.revision};
    const ref = workspaceRef(workspace);
    const publish = (update:(state:TargetState) => TargetState) =>
      setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound => ({...bound, target:update(bound.target)}))));
    hostCommand.current = label;
    setWorkspaces(list => applyIfCurrent(list, origin, item => ({
      ...updateBound(item, bound => ({...bound, target:beginTarget(bound.target, operation)})), busy:{key:label},
    })));
    async function refresh() {
      try {
        const view = await invoke<TargetView>('read_target', {workspace:ref});
        publish(state => readTarget(state, view));
      } catch (cause) {
        const error = fault(cause);
        publish(state => targetReadFailed(state, error));
      }
    }
    try {
      if (operation === 'read') await refresh();
      else if (ticket) {
        if (operation === 'check') {
          const response = await invoke<TargetCheckResponse>('check_target', {workspace:ref, expected:ticket.expected, configuration});
          publish(state => checkedTarget(state, ticket, response));
        } else if (operation === 'save') {
          const response = await invoke<TargetSaveResponse>('save_target', {workspace:ref, expected:ticket.expected, configuration, reviewedResolution:reviewedResolution ?? null});
          publish(state => savedTarget(state, ticket, response));
          await refresh();
        } else {
          const response = await invoke<TargetView>('remove_target', {workspace:ref, expected:ticket.expected});
          publish(state => removedTarget(state, ticket, response));
          await refresh();
        }
      }
    } catch (cause) {
      const error = fault(cause);
      if (ticket) publish(state => targetFailed(state, ticket, error));
    } finally {
      hostCommand.current = null;
      setWorkspaces(list => applyIfCurrent(list, origin, item => ({
        ...updateBound(item, bound => ({...bound, target:{...bound.target, operation:null}})), busy:null,
      })));
    }
  }
  async function chooseApplication(workspace:BoundWorkspace, field:TargetPickerField) {
    const target = workspace.bound.target;
    if (pickerRequest.current !== null || hostCommand.current !== null || !normalReady || closing || active
      || target.operation !== null || target.application.pending !== null || !targetTicket(target)) return;
    const started = beginApplicationPicker(target, field);
    if (!started.picker) return;
    const picker = started.picker;
    const origin:Origin = {id:workspace.id, revision:workspace.revision};
    pickerRequest.current = {picker};
    setPickerBusy(true);
    setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound => ({...bound, target:beginApplicationPicker(bound.target, field, picker)}))));
    try {
      const path = await invoke<string|null>('choose_target_application', {workspace:workspaceRef(workspace)});
      setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound => ({...bound, target:completeApplicationPicker(bound.target, picker, path)}))));
    } catch (cause) {
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound => ({...bound, target:completeApplicationPicker(bound.target, picker, null, error)}))));
    } finally {
      if (pickerRequest.current?.picker === picker) {
        pickerRequest.current = null;
        setPickerBusy(false);
      }
    }
  }

  async function checkRunningApplication(workspace:BoundWorkspace) {
    if (hostCommand.current !== null || pickerRequest.current !== null || applicationRequest.current !== null
      || closing || !normalReady || active) return;
    const ticket = eligibleRunningApplication(workspace.bound.target);
    if (!ticket) return;
    const requestId = crypto.randomUUID();
    const origin:Origin = {id:workspace.id, revision:workspace.revision};
    const ref = workspaceRef(workspace);
    applicationRequest.current = {workspace:ref, requestId};
    setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound =>
      ({...bound, target:beginRunningApplication(bound.target, ticket, requestId)}))));
    try {
      await invoke<void>('reserve_running_application', {workspace:ref, requestId});
      if (applicationRequest.current?.requestId !== requestId) {
        await invoke<boolean>('cancel_running_application', {workspace:ref, requestId});
        return;
      }
      const response = await invoke<TargetApplicationResponse>('check_running_application',
        {workspace:ref, expected:ticket.expected, requestId});
      setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound =>
        ({...bound, target:completeRunningApplication(bound.target, ticket, response)}))));
    } catch (cause) {
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound =>
        ({...bound, target:failRunningApplication(bound.target, ticket, requestId, error)}))));
    } finally {
      if (applicationRequest.current?.requestId === requestId) applicationRequest.current = null;
    }
  }

  function cancelRunningCheck(workspace:BoundWorkspace) {
    const requestId = workspace.bound.target.application.pending?.requestId;
    if (!requestId || applicationRequest.current?.requestId !== requestId) return;
    cancelApplicationRequest(workspace.id);
    const origin:Origin = {id:workspace.id, revision:workspace.revision};
    setWorkspaces(list => applyIfCurrent(list, origin, item => updateBound(item, bound =>
      ({...bound, target:cancelRunningApplication(bound.target, requestId)}))));
  }

  // Only the visible, inspected Run page reads its own owner lazily. Failed reads require an explicit retry.
  useEffect(() => {
    if (selected && isBound(selected) && selected.page === 'run' && !selected.bound.target.loaded
      && selected.bound.target.operation === null && commandReason === null && !closing) {
      void targetCommand(selected, 'read');
    }
  }, [selected?.id, selected?.revision, selected?.page, selected?.bound?.target.loaded, commandReason, closing]);

  function handlers(workspace: BoundWorkspace): RunHandlers {
    const ref = workspaceRef(workspace);
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const locked = commandReason !== null || closing;
    const bound = workspace.bound;
    const edit = (update: (bound: Bound) => Bound) => change(workspace.id, item => ({...updateBound(item, update), notice: null}));
    return {
      change: edit,
      target: {
        edit: draft => {
          cancelApplicationRequest(workspace.id);
          edit(current => ({...current, target:editTarget(current.target, draft)}));
        },
        reload: () => {void targetCommand(workspace, 'read');},
        check: () => {void targetCommand(workspace, 'check');},
        save: reviewed => {void targetCommand(workspace, 'save', reviewed);},
        remove: () => {void targetCommand(workspace, 'remove');},
        discard: () => {
          cancelApplicationRequest(workspace.id);
          edit(current => ({...current, target:discardTarget(current.target)}));
        },
        chooseApplication: field => {void chooseApplication(workspace, field);},
        checkApplication: () => {void checkRunningApplication(workspace);},
        cancelApplication: () => cancelRunningCheck(workspace),
      },
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
      recovery: recoveryHandlers(workspace),
      start: () => void startRun(workspace),
      stop: () => void stopRun(),
    };
  }

  async function startRun(workspace: BoundWorkspace) {
    const facts = derived[workspace.id];
    const bound = workspace.bound;
    if (hostCommand.current !== null || pickerRequest.current !== null || active || closing || leaseOwnerNow() !== null || facts.startBlock || facts.descriptorError) return;
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
      // A refused Start releases only its own preparation state; nothing else changes. A stale identity means the host
      // invalidated this selection (for example after Edit): re-read the listing so the Tab shows Reinspect, not Ready.
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, {id: workspace.id, revision: workspace.revision}, item => ({...item, error})));
      if (error.category === STALE_IDENTITY) void refreshOpenViews();
    } finally {
      epoch.current += 1;
      startInFlight.current = false;
      hostCommand.current = null;
      setStarting(null);
    }
  }

  // Edit host commands share the application's serialized command admission; typing never waits for them.
  async function authoringCall<T>(label: Command, call: () => Promise<T>): Promise<T> {
    const pendingCommand = hostCommand.current;
    if (pendingCommand !== null) throw new LocalFault({key: 'authoringWait', args: [pendingCommand]});
    hostCommand.current = label;
    authoringEpoch.current += 1;
    setAuthoringBusy(label);
    try {
      return await call();
    } finally {
      hostCommand.current = null;
      authoringEpoch.current += 1;
      setAuthoringBusy(null);
    }
  }

  function ownAuthoringWorker(action: () => Promise<void>): Promise<void> {
    if (authoringWorker.current !== null) return Promise.resolve();
    const settled = action().finally(() => {
      if (authoringWorker.current === settled) authoringWorker.current = null;
    });
    authoringWorker.current = settled;
    return settled;
  }

  // A fresh host listing: Tabs whose selection the host invalidated (another Tab bound to an edited source, or a stale
  // Start) move to their new revision and ask for Reinspect; untouched Tabs keep their drafts.
  async function refreshOpenViews() {
    try {
      const catalog = await invoke<WorkspaceCatalog>('workspace_catalog');
      setSavedClosed(catalog.closed);
      setCatalogFaults(catalog.faults);
      setCatalogError(null);
      setWorkspaces(list => applyInvalidatedViews(list, catalog.open));
    } catch (cause) {
      setCatalogError(fault(cause));
    }
  }

  // Why this Tab cannot enter Edit now. One lease exists per application and entering requires settled work.
  function editBlock(workspace: Workspace): string | null {
    if (closing) return t.applicationClosing;
    if (leaseOwnerId !== null) return ui.authoring.blockedOther(labelOf(leaseOwnerId));
    if (active) return ui.authoring.blockedActive;
    return workspace.busy !== null ? renderMessage(locale, workspace.busy) : commandReason;
  }

  // Open and Create never run package code or bind the Tab; the lease then excludes ordinary Start and Check.
  async function enterEdit(workspace: Workspace, path: string, packageId: string | null) {
    if (!(packageId === null ? path : packageId) || editBlock(workspace) !== null || leaseOwnerNow() !== null) return;
    invalidateOwnerTarget(workspace.id);
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    const ref = workspaceRef(workspace);
    change(workspace.id, item => ({...item, error: null, notice: null}));
    try {
      const view = await authoringCall(packageId === null ? 'openingPackage' : 'creatingPackage', () => packageId === null
        ? invoke<AuthoringView>('authoring_open', {workspace: ref, packagePath: path})
        : invoke<AuthoringView>('authoring_create', {workspace: ref, packageId}));
      updateAuthoring(() => openSession(view, {key: packageId === null ? 'authoringOpened' : 'authoringCreated'}));
      publishHostAuthoring(view.owner);
      const owner = view.owner.workspace.workspace_id;
      change(owner, item => ({...item, page: 'edit'}));
      go({kind: 'workspace', id: owner});
      await readRecognition();
    } catch (cause) {
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, origin, item => ({...item, error})));
    }
  }

  // Without a local view (for example after the WebView reloaded) the host's lease is rebuilt from disk.
  async function resumeAuthoring(owner: AuthoringRef) {
    const id = owner.workspace.workspace_id;
    try {
      const view = await authoringCall('refreshingPackage', () => invoke<AuthoringView>('authoring_refresh', {owner}));
      if (authoringStore.current === null) updateAuthoring(() => openSession(view, {key: 'authoringResumed'}));
      publishHostAuthoring(view.owner);
      change(id, item => ({...item, page: 'edit'}));
      await readRecognition();
    } catch (cause) {
      const error = fault(cause);
      change(id, item => ({...item, error}));
    }
    go({kind: 'workspace', id});
  }

  function returnToEdit() {
    const session = authoringStore.current;
    const owner = session?.owner ?? hostAuthoringRef.current ?? null;
    if (!owner) return;
    if (!session) {
      void resumeAuthoring(owner);
      return;
    }
    const id = owner.workspace.workspace_id;
    change(id, item => ({...item, page: 'edit'}));
    go({kind: 'workspace', id});
  }

  // Resolves true when the file is saved or has nothing left to save; a refusal keeps the draft and reports why.
  async function saveFile(path: string): Promise<boolean> {
    const session = authoringStore.current;
    if (!session || leaseLost) return false;
    if (saveBlock(session, path) === 'clean') return true;
    const ticket = saveTicket(session, path);
    if (!ticket) return false;
    updateAuthoring(current => current && current.owner.token === ticket.token ? beginPending(current, {kind: 'save', path}) : current);
    try {
      const mutation = await authoringCall('savingFile', () => invoke<AuthoringMutation>('authoring_save', {owner: session.owner, revision: ticket.expected, path, text: ticket.text}));
      updateAuthoring(current => applySave(current, ticket, mutation));
      if (session.recognition && mutation.view !== null && !await readRecognition()) return false;
      return true;
    } catch (cause) {
      updateAuthoring(current => failCommand(current, ticket.token, fault(cause)));
      return false;
    }
  }

  // Saves every savable dirty file in order and stops at the first failure; each ticket reads the latest revision.
  async function saveAll(): Promise<boolean> {
    const paths = authoringStore.current ? dirtyDrafts(authoringStore.current).filter(draft => !draft.missing).map(draft => draft.path) : [];
    for (const path of paths) {
      if (!await saveFile(path)) return false;
    }
    const session = authoringStore.current;
    if (session?.recognition && recognition.recognitionDirty(session.recognition) && !await saveRecognition()) return false;
    const latest = authoringStore.current;
    return latest === null || (dirtyDrafts(latest).length === 0 && (!latest.recognition || !recognition.recognitionDirty(latest.recognition)));
  }

  async function changeCatalog(edit: CatalogEdit): Promise<boolean> {
    const session = authoringStore.current;
    const ticket = session && !leaseLost ? catalogTicket(session, edit) : null;
    if (!session || !ticket) return false;
    updateAuthoring(current => current && current.owner.token === ticket.token ? beginPending(current, {kind: 'catalog'}) : current);
    try {
      const mutation = await authoringCall('changingCatalog', () => invoke<AuthoringMutation>('authoring_catalog', {owner: session.owner, revision: ticket.expected, edit}));
      updateAuthoring(current => applyCatalogMutation(current, ticket, mutation));
      if (session.recognition && mutation.view !== null) await readRecognition();
      return true;
    } catch (cause) {
      updateAuthoring(current => failCommand(current, ticket.token, fault(cause)));
      return false;
    }
  }

  function updateRecognition(token: string, update: (state: recognition.RecognitionState) => recognition.RecognitionState) {
    updateAuthoring(session => session?.owner.token === token && session.recognition
      ? {...session, recognition: update(session.recognition)} : session);
  }

  function acceptRecognitionView(owner: AuthoringRef, next: recognition.RecognitionView) {
    updateAuthoring(session => session && sameAuthoringRef(session.owner, owner) && sameAuthoringRef(next.owner, owner) && session.revision === next.revision
      ? {...session, recognition: session.recognition ? recognition.applyView(session.recognition, next) : recognition.openRecognition(next)}
      : session);
  }

  function recognitionFailed(token: string, cause: unknown) {
    const error = fault(cause);
    updateAuthoring(session => {
      const next = failCommand(session, token, error);
      return next?.owner.token === token && next.recognition
        ? {...next, recognition: recognition.failRecognition(next.recognition, token, error)} : next;
    });
  }

  // Local edits remain possible while a command awaits IPC; only the captured host draft is authorized.
  async function recognitionCommand(label: Command, action: (session: AuthoringSession) => Promise<void>): Promise<boolean> {
    const session = authoringStore.current;
    if (!session || session.pending !== null || leaseLost || closing) return false;
    const token = session.owner.token;
    updateAuthoring(current => current?.owner.token === token
      ? {...beginPending(current, {kind: 'recognition'}), recognition: current.recognition && recognition.clearRecognitionMessages(current.recognition)} : current);
    try {
      await authoringCall(label, () => action(session));
      return true;
    } catch (cause) {
      recognitionFailed(token, cause);
      return false;
    } finally {
      updateAuthoring(current => current?.owner.token === token && current.pending?.kind === 'recognition' ? {...current, pending: null} : current);
    }
  }

  async function readRecognition(): Promise<boolean> {
    return recognitionCommand('readingRecognition', async session => {
      const next = await invoke<recognition.RecognitionView>('recognition_view', {owner: session.owner, revision: session.revision});
      acceptRecognitionView(session.owner, next);
    });
  }

  async function checkRecognitionCapabilities() {
    const session = authoringStore.current;
    if (!session || session.pending !== null || leaseLost || closing || hostCommand.current !== null) return;
    const token = session.owner.token;
    capabilityOwner.current = token;
    updateAuthoring(current => current?.owner.token === token
      ? {...beginPending(current, {kind: 'recognition_trial'}), recognition: current.recognition && recognition.clearRecognitionMessages(current.recognition)} : current);
    expectedRun.current = null;
    epoch.current += 1;
    setStripMessage(null);
    try {
      const next = await invoke<recognition.RecognitionView>('recognition_capabilities', {owner: session.owner, revision: session.revision});
      acceptRecognitionView(session.owner, next);
    } catch (cause) {
      recognitionFailed(token, cause);
    } finally {
      updateAuthoring(current => current?.owner.token === token && current.pending?.kind === 'recognition_trial' ? {...current, pending: null} : current);
    }
  }

  async function openRecognition() {
    if (!authoringStore.current?.recognition && !await readRecognition()) return;
    const session = authoringStore.current;
    if (session && capabilityOwner.current !== session.owner.token) {
      await ownAuthoringWorker(checkRecognitionCapabilities);
    }
  }
  useEffect(() => {
    if (authoring?.destination === 'recognition' && authoring.recognition && authoring.pending === null
      && capabilityOwner.current !== authoring.owner.token) void ownAuthoringWorker(checkRecognitionCapabilities);
  }, [authoring?.destination, authoring?.owner.token, authoring?.recognition !== null, authoring?.pending]);

  async function synchronizeRecognition(owner: AuthoringRef): Promise<{session: AuthoringSession; state: recognition.RecognitionState}> {
    let session = authoringStore.current;
    if (!session || !sameAuthoringRef(session.owner, owner) || !session.recognition) throw new LocalFault({key: 'recognitionUnavailable'});
    if (recognitionConfigurationDirty.current || session.recognition.view.revision !== session.revision) {
      const next = await invoke<recognition.RecognitionView>('recognition_view', {owner, revision: session.revision});
      acceptRecognitionView(owner, next);
      recognitionConfigurationDirty.current = false;
      session = authoringStore.current;
      if (!session || !sameAuthoringRef(session.owner, owner) || !session.recognition) throw new LocalFault({key: 'recognitionUnavailable'});
    }
    const ticket = recognition.syncTicket(session.recognition);
    if (ticket) {
      const next = await invoke<recognition.RecognitionView>('recognition_update', {
        owner, revision: session.revision, document: ticket.document, frameId: ticket.frame_id, documentRevision: ticket.document_revision,
      });
      updateRecognition(owner.token, state => recognition.applySync(state, ticket, next));
    }
    session = authoringStore.current;
    if (!session || !sameAuthoringRef(session.owner, owner) || !session.recognition) throw new LocalFault({key: 'recognitionUnavailable'});
    if (!recognition.hostCurrent(session.recognition)) throw new LocalFault({key: 'recognitionDraftChanged'});
    return {session, state: session.recognition};
  }

  async function loadRecognition() {
    await recognitionCommand('loadingRecognition', async session => {
      const path = await invoke<string | null>('recognition_pick', {owner: session.owner, revision: session.revision});
      if (path === null) return;
      if (session.recognition) await synchronizeRecognition(session.owner);
      // Never leave the predecessor raster visible while replacement is pending or after its failure.
      updateRecognition(session.owner.token, state => recognition.applyView(state, {...state.view, frame: null}));
      await publishRecognitionPreview();
      try {
        const next = await invoke<recognition.RecognitionView>('recognition_load', {owner: session.owner, revision: session.revision, path});
        acceptRecognitionView(session.owner, next);
      } catch (cause) {
        // Replacement may have advanced the host revision before decoding failed. Reconcile only an explicit
        // no-frame view; a refused admission must not silently restore the predecessor preview.
        try {
          const next = await invoke<recognition.RecognitionView>('recognition_view', {owner: session.owner, revision: session.revision});
          if (next.frame === null) acceptRecognitionView(session.owner, next);
        } catch {/* The original load failure remains the visible outcome; Reload can reconcile later. */}
        throw cause;
      }
    });
  }

  async function confirmRecognition() {
    await recognitionCommand('updatingRecognition', async initial => {
      const {session, state} = await synchronizeRecognition(initial.owner);
      const ticket = recognition.confirmTicket(state);
      if (!ticket) throw new LocalFault({key: 'recognitionDraftChanged'});
      const next = await invoke<recognition.RecognitionView>('recognition_confirm', {
        owner: session.owner, revision: session.revision, frameId: ticket.frame_id, documentRevision: ticket.document_revision,
      });
      updateRecognition(ticket.token, current => recognition.applyConfirm(current, ticket, next));
    });
  }

  async function trialRecognition(kind: 'frame' | 'sample', ids: string[]) {
    const initial = authoringStore.current;
    if (!initial || initial.pending !== null || leaseLost || closing || hostCommand.current !== null) return;
    const token = initial.owner.token;
    let ticket: recognition.TrialTicket | null = null;
    updateAuthoring(current => current?.owner.token === token
      ? {...beginPending(current, {kind: 'recognition'}), recognition: current.recognition && recognition.clearRecognitionMessages(current.recognition)} : current);
    try {
      const {session, state} = await authoringCall('updatingRecognition', () => synchronizeRecognition(initial.owner));
      ticket = recognition.trialTicket(state, kind, ids);
      if (!ticket) throw new LocalFault({key: 'recognitionDraftChanged'});
      const captured = ticket;
      updateAuthoring(current => current?.owner.token === token && current.recognition
        ? {...current, pending: {kind: 'recognition_trial'}, recognition: recognition.beginTrial(current.recognition, captured)} : current);
      expectedRun.current = null;
      epoch.current += 1;
      setStripMessage(null);
      const result = await invoke<recognition.RecognitionTrial>('recognition_trial', {
        owner: session.owner, revision: session.revision, frameId: ticket.frame_id, documentRevision: ticket.document_revision,
        selectedIds: ticket.selected_ids, sampleId: ticket.sample_id,
      });
      const retained = recognitionConfigurationDirty.current ? {...result, stale: true} : result;
      updateRecognition(token, current => recognition.applyTrial(current, captured, retained));
    } catch (cause) {
      if (ticket) {
        const captured = ticket;
        updateRecognition(token, current => recognition.applyTrial(current, captured, null, fault(cause)));
      }
      recognitionFailed(token, cause);
    } finally {
      updateAuthoring(current => current?.owner.token === token ? {...current, pending: null} : current);
    }
  }

  async function saveRecognition(): Promise<boolean> {
    const initial = authoringStore.current;
    if (!initial?.recognition || !recognition.recognitionDirty(initial.recognition)) return true;
    if (initial.refreshRequired || initial.conflict) return false;
    if (dirtyDrafts(initial).some(draft => draft.kind === 'manifest')) {
      recognitionFailed(initial.owner.token, new LocalFault({key: 'recognitionManifestDirty'}));
      return false;
    }
    return recognitionCommand('savingRecognition', async current => {
      const {session, state} = await synchronizeRecognition(current.owner);
      const ticket = recognition.saveTicket(state);
      if (!ticket) throw new LocalFault({key: 'recognitionDraftChanged'});
      const result = await invoke<{mutation: AuthoringMutation; recognition: recognition.RecognitionView | null}>('recognition_save', {
        owner: session.owner, revision: session.revision, documentRevision: ticket.document_revision, cropIds: ticket.crop_ids,
      });
      updateAuthoring(latest => {
        const next = applyRecognitionMutation(latest, result.mutation);
        return next?.owner.token === ticket.token && next.recognition
          ? {...next, recognition: recognition.applySave(next.recognition, ticket, result.recognition)} : next;
      });
    });
  }

  async function discardRecognition() {
    await recognitionCommand('updatingRecognition', async session => {
      const sent = session.recognition;
      const next = await invoke<recognition.RecognitionView>('recognition_discard', {owner: session.owner, revision: session.revision});
      updateRecognition(session.owner.token, state => {
        const unchanged = sent && state.localRevision === sent.localRevision
          && state.cropIds.length === sent.cropIds.length && state.cropIds.every(id => state.cropMarks[id] === sent.cropMarks[id]);
        return unchanged ? recognition.applyDiscard(state, next) : recognition.applyView(state, next, null);
      });
    });
  }

  async function copyRecognition(id: string, mode: recognition.SnippetKind) {
    await recognitionCommand('copyingRecognition', async initial => {
      const {session, state} = await synchronizeRecognition(initial.owner);
      const ticket = recognition.copyTicket(state, id, mode);
      if (!ticket) throw new LocalFault({key: 'recognitionDraftChanged'});
      const captured = ticket;
      let result: recognition.CopyResult | null = null;
      try {
        result = await invoke<recognition.CopyResult>('recognition_copy', {
          owner: session.owner, revision: session.revision, documentRevision: ticket.document_revision, definitionId: id, mode,
        });
        updateRecognition(captured.token, current => recognition.applyCopy(current, captured, result, null));
        updateAuthoring(current => current?.owner.token === captured.token ? {...current, notice: {key: 'recognitionCopied'}} : current);
      } catch (cause) {
        updateRecognition(captured.token, current => recognition.applyCopy(current, captured, result, fault(cause)));
        throw cause;
      }
    });
  }

  async function openRecognitionPreview() {
    const session = authoringStore.current;
    if (!session || leaseLost || closing || choice !== null || choiceRunning.current) return;
    try {
      await invoke<void>('recognition_open_preview', {owner: session.owner, revision: session.revision});
      await publishRecognitionPreview();
    } catch (cause) {
      const error = fault(cause);
      updateAuthoring(current => current?.owner.token === session.owner.token
        ? {...current, error, recognition: current.recognition ? recognition.failRecognition(current.recognition, session.owner.token, error) : null}
        : current);
    }
  }

  async function publishRecognitionPreview() {
    if (!previewConnected.current) return;
    const session = authoringStore.current;
    const editable = !closing && choice === null && !choiceRunning.current && !leaseLost && session?.pending?.kind !== 'exit' && session?.pending?.kind !== 'duplicate';
    const running = session?.pending?.kind === 'recognition_trial' || session?.pending?.kind === 'validate'
      || (busy(view.state) && (view.operation === 'authoring_validate' || view.operation?.startsWith('recognition_') === true));
    const snapshot = session?.recognition ? recognition.previewSnapshot(session.recognition, locale, editable, editable ? null : ui.authoring.block('pending'), running) : null;
    try {await emitTo(recognition.PREVIEW_LABEL, recognition.PREVIEW_STATE, snapshot);}
    catch {
      previewConnected.current = false;
      if (session) updateAuthoring(current => current?.owner.token === session.owner.token
        ? {...current, error: new LocalFault({key: 'recognitionPreviewFailed'})} : current);
    }
  }

  previewBridge.current = {
    ready: () => {previewConnected.current = true; void publishRecognitionPreview();},
    edit: message => {
      const session = authoringStore.current;
      if (!session || leaseLost || closing || choice !== null || choiceRunning.current || session.pending?.kind === 'exit' || session.pending?.kind === 'duplicate') return;
      updateRecognition(session.owner.token, state => recognition.applyPreviewEdit(state, message, ui.recognition.defaultName));
      void publishRecognitionPreview();
    },
  };
  useEffect(() => {
    let alive = true;
    const stops: (() => void)[] = [];
    const register = (promise: Promise<() => void>) => {
      void promise.then(stop => {if (alive) stops.push(stop); else stop();}).catch(cause => {
        const token = authoringStore.current?.owner.token;
        if (alive && token) recognitionFailed(token, cause);
      });
    };
    register(listen(recognition.PREVIEW_READY, () => previewBridge.current.ready()));
    register(listen<recognition.PreviewEditMessage>(recognition.PREVIEW_EDIT, event => previewBridge.current.edit(event.payload)));
    register(listen('recognition-preview-closed', () => {previewConnected.current = false;}));
    return () => {alive = false; for (const stop of stops) stop();};
  }, []);
  useEffect(() => {void publishRecognitionPreview();}, [authoring?.recognition, authoring?.pending?.kind, view.state, view.operation, locale, closing, leaseLost, choice, choiceBusy]);
  useEffect(() => {
    const owner = authoring?.owner;
    return () => {
      if (owner) void invoke<void>('recognition_close_preview', {owner}).catch(() => {});
    };
  }, [authoring?.owner.token]);

  async function refreshAuthoring() {
    const session = authoringStore.current;
    if (!session || session.pending !== null) return;
    const token = session.owner.token;
    updateAuthoring(current => current && current.owner.token === token ? beginPending(current, {kind: 'refresh'}) : current);
    try {
      const view = await authoringCall('refreshingPackage', () => invoke<AuthoringView>('authoring_refresh', {owner: session.owner}));
      updateAuthoring(current => applyRefresh(current, view));
      await readRecognition();
    } catch (cause) {
      updateAuthoring(current => failCommand(current, token, fault(cause)));
    }
  }

  // Validation reserves the shared work slot under the lease; its child shows in the strip with an owner-bound Stop.
  async function validateAuthoring() {
    const session = authoringStore.current;
    if (!session || leaseLost || hostCommand.current !== null || active || closing) return;
    const ticket = validationTicket(session);
    if (!ticket) return;
    updateAuthoring(current => current && current.owner.token === ticket.token ? beginPending(current, {kind: 'validate'}) : current);
    // The child replaces any earlier run in the shared controller view; accept its host-issued ID when polled.
    expectedRun.current = null;
    epoch.current += 1;
    setStripMessage(null);
    try {
      const result = await invoke<AuthoringValidation>('authoring_validate', {owner: session.owner, revision: ticket.revision});
      updateAuthoring(current => applyValidation(current, ticket, result));
    } catch (cause) {
      updateAuthoring(current => failCommand(current, ticket.token, fault(cause)));
    }
  }

  // Cancellation stays independent of command admission, like Stop; the reply only acknowledges the request.
  async function stopValidation() {
    const owner = authoringStore.current?.owner ?? hostAuthoringRef.current;
    if (!owner) return;
    const token = owner.token;
    setStopping(true);
    try {
      await invoke<boolean>('authoring_stop', {owner});
      updateAuthoring(current => current && current.owner.token === token ? {...current, notice: {key: 'authoringStopRequested'}} : current);
      setStripMessage({text: {key: 'authoringStopRequested'}, error: false});
    } catch (cause) {
      const error = fault(cause);
      updateAuthoring(current => current && current.owner.token === token ? {...current, error} : current);
      setStripMessage({text: {key: 'stopFailed', args: [faultSummary(error, true)]}, error: true});
    } finally {
      setStopping(false);
    }
  }

  async function recoverAuthoringPackage() {
    const session = authoringStore.current;
    if (!session || session.pending !== null) return;
    const token = session.owner.token;
    try {
      await authoringCall('recoveringPackage', () => invoke<void>('authoring_recover', {packagePath: session.packagePath}));
      updateAuthoring(current => current && current.owner.token === token ? {...current, error: null} : current);
      await refreshAuthoring();
    } catch (cause) {
      updateAuthoring(current => failCommand(current, token, fault(cause)));
    }
  }

  async function recoverWorkspacePackage(workspace: Workspace, packagePath: string) {
    const origin: Origin = {id: workspace.id, revision: workspace.revision};
    try {
      await authoringCall('recoveringPackage', () => invoke<void>('authoring_recover', {packagePath}));
      setWorkspaces(list => applyIfCurrent(list, origin, item => ({...item, error: null, notice: {key: 'authoringRecovered'}})));
    } catch (cause) {
      const error = fault(cause);
      setWorkspaces(list => applyIfCurrent(list, origin, item => ({...item, error})));
    }
  }

  // Duplicate reads the saved revision, so the choice dialog has already saved or deliberately left the drafts.
  async function duplicateAuthoring(packageId: string): Promise<boolean> {
    const session = authoringStore.current;
    if (!session || session.pending !== null || leaseLost) return false;
    const token = session.owner.token;
    updateAuthoring(current => current && current.owner.token === token ? beginPending(current, {kind: 'duplicate'}) : current);
    try {
      const view = await authoringCall('duplicatingPackage', () => invoke<AuthoringView>('authoring_duplicate', {owner: session.owner, revision: session.revision, packageId}));
      updateAuthoring(current => current && current.owner.token === token ? openSession(view, {key: 'authoringDuplicated', args: [view.package_path, view.package_id]}) : current);
      publishHostAuthoring(view.owner);
      await readRecognition();
      return true;
    } catch (cause) {
      updateAuthoring(current => failCommand(current, token, fault(cause)));
      return false;
    }
  }

  // Ends the lease and applies the owner's invalidated view; the Tab then needs an explicit Inspect/Reinspect. Returns
  // 'released' when the host no longer held this view's lease, and null when the host refused (the view stays).
  async function exitAuthoring(): Promise<WorkspaceView | 'released' | null> {
    const session = authoringStore.current;
    const owner = session?.owner ?? hostAuthoringRef.current ?? null;
    if (!owner) return null;
    const ownerId = owner.workspace.workspace_id;
    if (session && hostAuthoringRef.current === null && session.pending === null && authoringBusy === null) {
      updateAuthoring(() => null);
      change(ownerId, item => ({...item, page: 'run'}));
      return 'released';
    }
    const token = owner.token;
    updateAuthoring(current => current && current.owner.token === token ? beginPending(current, {kind: 'exit'}) : current);
    try {
      return await authoringCall('exitingEdit', async () => {
        const view = await invoke<WorkspaceView>('authoring_exit', {owner});
        updateAuthoring(current => current && current.owner.token === token ? null : current);
        publishHostAuthoring(null);
        const packagePath = session?.packagePath ?? null;
        setWorkspaces(list => updateWorkspace(list, view.workspace_id, item => applyAuthoringExit(item, view, packagePath)));
        await refreshOpenViews();
        return view;
      });
    } catch (cause) {
      const error = fault(cause);
      if (session) updateAuthoring(current => failCommand(current, token, error));
      else change(ownerId, item => ({...item, error}));
      return null;
    }
  }

  function requestChoice(next: Choice) {
    if (choiceRunning.current) return;
    const session = authoringStore.current;
    if (session && (dirtyDrafts(session).length > 0 || (session.recognition && recognition.recognitionDirty(session.recognition)))) setChoice(next);
    else void resolveChoice(next, false);
  }

  // Save stops at the first failed file; any refusal keeps the lease, drafts and selection and shows the editor.
  async function resolveChoice(intent: Choice, save: boolean) {
    if (choiceRunning.current) return;
    choiceRunning.current = true;
    setChoiceBusy(true);
    let done = false;
    try {
      const worker = authoringWorker.current;
      if (worker) {
        await stopValidation();
        // The operation handler retains its primary/cleanup outcome before this promise settles.
        await worker.catch(() => {});
      }
      if (save && !await saveAll()) return;
      if (intent.kind === 'duplicate') {
        done = await duplicateAuthoring(intent.packageId);
        return;
      }
      if (intent.kind === 'close') {
        done = await closeApplication(authoringStore.current?.owner ?? hostAuthoringRef.current ?? null);
        return;
      }
      const packagePath = authoringStore.current?.packagePath ?? null;
      const exited = await exitAuthoring();
      if (exited === null) return;
      done = true;
      if (intent.kind === 'closeTab') {
        const current = openWorkspaces.current.find(item => item.id === intent.workspaceId);
        if (current) void closeTab(exited === 'released' ? current : applyAuthoringExit(current, exited, packagePath), false);
      }
    } finally {
      choiceRunning.current = false;
      setChoiceBusy(false);
      setChoice(null);
      if (!done && authoringStore.current) returnToEdit();
    }
  }

  const editHandlers: EditHandlers = {
    select: (path, previous) => updateAuthoring(session => session && selectFile(session, path, previous)),
    recognition: previous => {
      updateAuthoring(session => session && selectRecognition(session, previous));
      void openRecognition();
    },
    edit: (path, next, before, input) => updateAuthoring(session => session && editFile(session, path, next, before, input)),
    replace: (path, text, typed) => updateAuthoring(session => session && replaceFile(session, path, text, typed)),
    compositionStart: (path, range) => updateAuthoring(session => session && beginComposition(session, path, range)),
    compositionEnd: path => updateAuthoring(session => session && endComposition(session, path)),
    range: (path, range) => updateAuthoring(session => session && recordRange(session, path, range)),
    undo: path => updateAuthoring(session => session && undoFile(session, path)),
    redo: path => updateAuthoring(session => session && redoFile(session, path)),
    reveal: (path, range) => updateAuthoring(session => session && revealRange(session, path, range)),
    discard: path => updateAuthoring(session => session && discardFile(session, path)),
    save: path => void saveFile(path),
    saveAll: () => void saveAll(),
    validate: () => void ownAuthoringWorker(validateAuthoring),
    stopValidation: () => void stopValidation(),
    refresh: () => void refreshAuthoring(),
    recover: () => void recoverAuthoringPackage(),
    catalog: edit => changeCatalog(edit),
    duplicate: packageId => requestChoice({kind: 'duplicate', packageId}),
    exit: () => requestChoice({kind: 'exit'}),
  };

  function pageAuthoring(workspace: Workspace): PageAuthoring {
    return {
      role: leaseOwnerId === null ? null : leaseOwnerId === workspace.id ? 'owner' : 'other',
      ownerLabel: leaseLabel, block: editBlock(workspace), loaded: authoring !== null,
      onOpen: path => void enterEdit(workspace, path, null),
      onCreate: () => void enterEdit(workspace, '', workspace.editPackageId.trim()),
      onReturn: returnToEdit,
      onPath: value => change(workspace.id, item => ({...item, editPath: value, error: null})),
      onPackageId: value => change(workspace.id, item => ({...item, editPackageId: value, error: null})),
      onRecover: path => void recoverWorkspacePackage(workspace, path),
    };
  }

  const envParsed = useMemo(() => readEnvironment(settingsDraft.environment, locale), [settingsDraft.environment, locale]);
  const envDirty = Object.keys(envParsed.errors).length > 0 || !sameEnvironment(envParsed.environment, savedEnvironment);
  const parsedSettings = useMemo(() => readSettingsDraft(settingsDraft, locale), [settingsDraft, locale]);
  const dialogDirty = settings === null || settingsDraft.locale !== settings.locale || settingsDraft.logLimit.trim() !== String(settings.gui_log_limit)
    || !sameNotifications(settingsDraft.notifications, settings.notifications) || settingsDraft.backupDirectory.trim() !== (settings.backup_directory ?? '')
    || settingsDraft.packagesRoot.trim() !== (settings.packages_root ?? '') || envDirty;
  // Check binds the workspace visible when the dialog opened; an unbound Tab is never passed as a package association.
  const checkTarget: CheckTarget = selected?.bound
    ? {workspace: workspaceRef(selected), label: workspaceLabel(selected, workspaces), descriptorPath: selected.bound.descriptorPath.trim() || null, packageInventoryIdentity: selected.bound.package.inventory_identity}
    : {workspace: null, label: t.none, descriptorPath: null, packageInventoryIdentity: null};
  const checkStale = lastCheck ? staleReasons(lastCheck.association, {
    saved: savedEnvironment, draftDirty: envDirty, workspace: checkTarget.workspace, descriptorPath: checkTarget.descriptorPath, packageInventoryIdentity: checkTarget.packageInventoryIdentity,
  }, locale) : [];

  // Check reads saved settings only; the association records exactly what the backend will read.
  async function checkEnvironment() {
    if (hostCommand.current !== null || pickerRequest.current !== null || active || closing || envDirty || appBusy || leaseOwnerNow() !== null) return;
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
    onRetry: () => void bootstrapAction('retrying', () => invoke<BootstrapStatus>('retry_bootstrap', {discard: bootstrap.restore.retryDiscard}), true),
    onImportRoot: () => void bootstrapAction('importingRoot', () => invoke<BootstrapStatus>('import_legacy_root'), true),
    onSnapshot: () => void snapshot(bootstrap.snapshotDestination.trim() || null),
    onRestore: () => void bootstrapAction('restoring', () => invoke<BootstrapStatus>('restore_snapshot', {
      archivePath: bootstrap.restore.archivePath.trim(), receiptGeneration, confirm: bootstrap.restore.confirm, discard: bootstrap.restore.discard,
    }), true),
    onRecover: (rollback: boolean) => void bootstrapAction('recovering', () => invoke<BootstrapStatus>('recover_restore', {rollback, confirm: bootstrap.restore.recoverConfirm, discard: bootstrap.restore.recoverDiscard}), true),
    onExit: () => void exitApplication(),
    onDismiss: () => {setConfigurationOpen(false); menuButton.current?.focus();},
  };

  // Closing resolves drafts first; shutdown retains the lease until owned work settles.
  async function exitApplication() {
    setMenuOpen(false);
    if (leaseOwnerNow() !== null) {
      requestChoice({kind: 'close'});
      return;
    }
    invalidateAllTargets();
    setClosing(true);
    try {await getCurrentWindow().close();} catch (cause) {dispatch({type: 'actionFailed', fault: fault(cause)}); setClosing(false);}
  }

  // Explicit draft resolution also permits shutdown from Recovery or a contained validation failure.
  async function closeApplication(owner: AuthoringRef | null = null): Promise<boolean> {
    invalidateAllTargets();
    setClosing(true);
    try {
      await invoke<void>('app_close', {owner});
      return true;
    } catch (cause) {
      dispatch({type: 'actionFailed', fault: fault(cause)});
      setClosing(false);
      return false;
    }
  }

  // The host prevents native window close and OS exit while a lease exists and asks here instead.
  const closeRequested = useRef(() => {});
  closeRequested.current = () => {
    if (closing || choiceRunning.current) return;
    if (leaseOwnerNow() !== null) requestChoice({kind: 'close'});
    else void closeApplication();
  };
  useEffect(() => {
    let alive = true;
    let stop: (() => void) | null = null;
    void listen('authoring-close-requested', () => closeRequested.current()).then(unlisten => {
      if (alive) stop = unlisten; else unlisten();
    }).catch(() => {});
    return () => {alive = false; stop?.();};
  }, []);
  useEffect(() => {
    let alive = true;
    let stop: (() => void) | null = null;
    void listen<Fault>('application-close-refused', event => {
      dispatch({type: 'actionFailed', fault: event.payload});
      setClosing(false);
    }).then(unlisten => {if (alive) stop = unlisten; else unlisten();}).catch(() => {});
    return () => {alive = false; stop?.();};
  }, []);

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
    // The Edit owner closes only after its drafts are resolved and the lease has ended.
    if (leaseOwnerNow() === workspace.id) {requestChoice({kind: 'closeTab', workspaceId: workspace.id}); return;}
    if (dirtyDraft(workspace) && !confirmed) {setPendingClose(workspace.id); return;}
    invalidateOwnerTarget(workspace.id);
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

  const awaitingAuthoringWorker = authoringWorkerPending && !busy(view.state);
  const owner = authoringWorkerPending ? leaseOwnerId : starting ? starting.workspaceId : view.workspace_id;
  const phase = starting || awaitingAuthoringWorker ? 'preparing' : view.state;
  const stripKind = awaitingAuthoringWorker ? ui.operation(authoring?.pending?.kind === 'validate' ? 'authoring_validate'
    : authoring?.recognition?.running ? 'recognition_trial' : 'recognition_capabilities')
    : starting ? ui.operation(starting.kind === 'check' ? 'environment_check' : 'run') : ui.operation(view.operation);
  const editVisible = selected !== undefined && selected.id === leaseOwnerId && selected.page === 'edit' && authoring !== null;
  // Every surface, dialogs included, keeps the operation's Stop and the Edit owner's Return to Edit reachable.
  const strip = (idPrefix: string, onReturn: () => void = returnToEdit): ReactNode => <>
    {active && <OperationStrip idPrefix={idPrefix} owner={owner === null ? t.applicationOwner : labelOf(owner)} kind={stripKind} phase={phase} run={starting || awaitingAuthoringWorker ? null : view.run}
      message={stripMessage && {text: renderMessage(locale, stripMessage.text), error: stripMessage.error}}
      stopDisabled={stopping || closing || (!authoringWorkerPending && (!view.run || !busy(view.state) || view.state === 'stopping' || starting !== null))}
      onStop={() => void (authoringWorkerPending || validationActive || recognitionActive ? stopValidation() : stopRun())}/>}
    {leaseOwnerId !== null && <AuthoringStrip idPrefix={idPrefix} owner={labelOf(leaseOwnerId)} packageId={authoring?.packageId ?? null} unsaved={unsavedCount}
      showReturn={!(idPrefix === 'app' && editVisible)} onReturn={onReturn}/>}
  </>;

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
        : <BootstrapPage ui={bootstrap} status={status} dispatch={dispatch} handlers={bootstrapHandlers} admission={admission} exiting={closing} inShell={false}/>}
    </LocaleContext>;
  }

  const workspaceItems: WorkspaceOption[] = workspaces.map(workspace => {
    const current = runView(workspace);
    const facts = derived[workspace.id];
    const owns = starting?.workspaceId === workspace.id || (current.live && busy(current.view.state));
    const sourceIssue = workspace.sourceError ? workspace.sourceError.category === UNSUPPORTED_SOURCE ? t.unsupportedSource : t.unavailableSource : null;
    const issue = workspace.error?.category ?? (workspace.recovery?.view.binding_required ? t.bindingRequired : null)
      ?? workspace.bound?.profilesError?.category ?? workspace.bound?.target.readError?.category ?? workspace.bound?.target.issue?.fault.category
      ?? sourceIssue ?? (workspace.recovery !== null && workspace.recovery.view.profiles.length > 0 ? t.repairRequired : null)
      ?? (facts ? facts.numericErrors ? t.invalidFields : !facts.bound ? t.staleProfile : facts.descriptorError ? t.invalidDescriptor : null : null);
    const attention = issue !== null || needsAttention(current.view);
    const optionStatus: WorkspaceOption['status'] = owns ? {kind: 'busy', text: `${ui.phase(starting?.workspaceId === workspace.id ? 'preparing' : current.view.state)} · ${ui.operation(starting?.workspaceId === workspace.id ? starting.kind === 'check' ? 'environment_check' : 'run' : current.view.operation)}`}
      : workspace.busy ? {kind: 'busy', text: renderMessage(locale, workspace.busy)}
      : attention ? {kind: 'attention', text: t.attention(issue ?? current.view.error?.category ?? text(current.view.result?.status) ?? t.unresolved)}
      : leaseOwnerId === workspace.id ? unsavedCount > 0 ? {kind: 'dirty', text: t.editingUnsaved} : {kind: 'ready', text: t.editing}
      : dirtyDraft(workspace) ? {kind: 'dirty', text: t.unsavedDraft}
      : facts ? {kind: 'ready', text: t.ready(facts.selectedProfile ? facts.selectedProfile.name : t.draft)}
      : {kind: 'ready', text: t.noPackage};
    return {id: workspace.id, title: workspace.bound?.packagePath ?? workspace.internalName, label: workspaceLabel(workspace, workspaces), status: optionStatus, running: owns, attention};
  });
  const inShellRecovery = status !== null && status.state === 'recovery';
  // Guidance names the owner's scope truthfully: an Application-scoped check has no owning workspace.
  const activeOwner = (workspace: Workspace): ActiveOwner => !active || owner === workspace.id ? null : owner === null ? 'application' : 'other';

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
          <button id="page-run" type="button" className="nav-item" aria-current={selected.page === 'run' ? 'page' : undefined} onClick={() => {setReveal(null); change(selected.id, item => ({...item, page: 'run'}));}}>{isBound(selected) ? t.runControl : t.guidance}</button>
          <button id="page-logs" type="button" className="nav-item" aria-current={selected.page === 'logs' ? 'page' : undefined} onClick={() => {setReveal(null); change(selected.id, item => ({...item, page: 'logs'}));}}>{t.logs}<span className="count">{logCounts[selected.id] ?? 0}</span></button>
          {selected.id === leaseOwnerId && <button id="page-edit" type="button" className="nav-item" aria-current={selected.page === 'edit' && authoring !== null ? 'page' : undefined}
            onClick={() => {setReveal(null); returnToEdit();}}>{t.edit}{unsavedCount > 0 && <span className="count">{unsavedCount}</span>}</button>}
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
    {(inShellRecovery || configurationOpen) && status && <BootstrapPage ui={bootstrap} status={status} dispatch={dispatch} handlers={bootstrapHandlers} admission={admission} exiting={closing} inShell={true}/>}
    {status?.state === 'ready' && status.fault && <div className="content-wide"><FaultMessage title={`${ui.bootstrap.stage} · ${status.stage}`} value={status.fault}/>
      <div className="button-row"><button id="refresh-status" type="button" disabled={bootstrap.pending !== null} onClick={() => void bootstrapAction('refreshingStatus', () => invoke<BootstrapStatus>('bootstrap_status'), false)}>{ui.reopen.refresh}</button>
        <span className="muted">{ui.bootstrap.actionFailedHelp}</span></div></div>}
    <div className="workspace">
      <main id="workspace-panel" className="content" aria-label={selected ? t.workspaceAria(workspaceLabel(selected, workspaces)) : undefined}>
        {selected && selected.page === 'edit' && editVisible && authoring && <EditPage key={authoring.owner.token} session={authoring} label={workspaceLabel(selected, workspaces)}
          handlers={editHandlers} locked={commandReason !== null || closing} lockReason={commandReason ?? (closing ? t.applicationClosing : null)}
          packagesRoot={packagesRoot} leaseLost={leaseLost} validationActive={validationActive} recognitionDirty={recognitionDirty}
          recognition={authoring.recognition
            ? <RecognitionPage state={authoring.recognition} locked={commandReason !== null || closing || authoring.pending !== null}
              lockReason={commandReason ?? (closing ? t.applicationClosing : authoring.pending !== null ? ui.authoring.block('pending') : null)}
              leaseLost={leaseLost} trialActive={recognitionActive}
              onState={update => {
                if (!closing && choice === null && !choiceRunning.current && !leaseLost) updateRecognition(authoring.owner.token, update);
              }}
              handlers={{
                load: () => void loadRecognition(), confirm: () => void confirmRecognition(),
                trial: (kind, ids) => void ownAuthoringWorker(() => trialRecognition(kind, ids)), stop: () => void stopValidation(),
                save: () => void saveRecognition(), copy: (id, mode) => void copyRecognition(id, mode),
                discard: () => void discardRecognition(), capabilities: () => void ownAuthoringWorker(checkRecognitionCapabilities),
                openPreview: () => void openRecognitionPreview(), reload: () => void readRecognition(),
              }}/>
            : <div className="button-row"><span role="status">{t.recognitionUnavailable}</span>
              <button id="recognition-reload" type="button" disabled={authoring.pending !== null || leaseLost || closing}
                onClick={() => void readRecognition()}>{ui.authoring.refresh}</button></div>}/>}
        {selected && (selected.page === 'run' || (selected.page === 'edit' && !editVisible)) && (isBound(selected)
          ? <RunPage key={`${selected.id}:${selected.revision}`} workspace={selected} label={workspaceLabel(selected, workspaces)} derived={derived[selected.id]} run={runView(selected)} snapshot={operation?.snapshot ?? null}
            locked={commandReason !== null || closing} active={active} pickerBusy={pickerBusy} starting={starting?.workspaceId === selected.id} stopping={stopping} closing={closing}
            savedEnvironment={savedEnvironment} handlers={handlers(selected)} authoring={pageAuthoring(selected)}/>
          : <GuidancePage key={`${selected.id}:${selected.revision}`} workspace={selected} label={workspaceLabel(selected, workspaces)} locked={commandReason !== null || closing} lockReason={commandReason ?? (closing ? t.applicationClosing : null)}
            onPath={value => change(selected.id, item => ({...item, inspectPath: value, error: null}))} onInspect={() => inspectFor(selected)} activeOwner={activeOwner(selected)}
            recovery={recoveryHandlers(selected)} authoring={pageAuthoring(selected)} packagesRoot={packagesRoot}/>)}
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
      busyReason={appBusy === 'creatingWorkspace' ? null : commandReason} strip={strip('create', () => {if (appBusy !== 'creatingWorkspace') {setCreateOpen(false); returnToEdit();}})}/>
    <SavedWorkspacesDialog open={savedOpen} onCancel={() => {if (reopening === null) setSavedOpen(false);}} closed={savedClosed} faults={catalogFaults} listError={catalogError}
      openCount={workspaces.length} savedCount={savedCount} onRefresh={() => void refreshSaved()} onReopen={name => void reopenWorkspace(name)} reopening={reopening} reopenError={reopenError}
      busyReason={appBusy === 'reopeningWorkspace' ? null : commandReason} strip={strip('saved', () => {if (reopening === null) {setSavedOpen(false); returnToEdit();}})}/>
    <SettingsDialog open={dialogOpen} onCancel={() => setDialogOpen(false)} settings={settings} draft={settingsDraft} onDraft={next => {setSettingsDraft(next); setSaveNotice(null);}}
      defaultPackagesRoot={defaultPackagesRoot}
      parsed={parsedSettings} dirty={dialogDirty} saving={appBusy === 'savingSettings'} busyReason={commandReason} saveError={dialogError?.kind === 'save' ? dialogError.value : null} saveNotice={renderMessage(locale, saveNotice)} onSave={() => void saveSettings()}
      envDirty={envDirty} active={active} pickerBusy={pickerBusy} target={checkTarget} onCheck={() => void checkEnvironment()} lastCheck={lastCheck} stale={checkStale} originLabel={labelOf}
      checkError={dialogError?.kind === 'check' ? dialogError.value : null} authoringReason={authoringReason}
      retained={logs.items.length} evicted={logs.evicted}
      onSnapshot={() => void snapshot(null)} snapshotPending={bootstrap.snapshotPending} snapshotOutcome={bootstrap.snapshotOutcome} strip={strip('dialog', () => {if (appBusy !== 'savingSettings') {setDialogOpen(false); returnToEdit();}})}/>
    <DirtyChoiceDialog intent={choice?.kind ?? null} drafts={authoring ? dirtyDrafts(authoring) : []} recognitionDirty={recognitionDirty} busy={choiceBusy} saveBlock={leaseLost ? ui.authoring.leaseLost : null}
      onSave={() => {if (choice) void resolveChoice(choice, true);}} onDiscard={() => {if (choice) void resolveChoice(choice, false);}}
      onCancel={() => {if (!choiceBusy) setChoice(null);}}/>
  </div></LocaleContext>;
}

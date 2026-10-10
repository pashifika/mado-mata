import {dirtyDrafts, sameAuthoringRef, validationCurrent} from './authoring.ts';
import type {AuthoringSession} from './authoring.ts';
import {LocalFault, messages, renderMessage} from './i18n.ts';
import type {Locale} from './i18n.ts';
import type {RecognitionTrial} from './recognition.ts';
import {cleanupLabel, faultSummary, record, text} from './state.ts';
import type {AuthoringRef, ControllerView, Fault, NativeSelectionView} from './types.ts';
import {currentTargetDraft} from './target.ts';
import {busy, needsAttention} from './workspace.ts';
import type {Derived, Workspace} from './workspace.ts';

export interface StatusItem {
  id: string;
  severity: 'error' | 'warning' | 'progress' | 'info';
  text: string;
  action: 'run' | 'execution' | 'edit' | null;
  earlierDraft?: boolean;
}

export function scopedAuthoring(workspace: Workspace | undefined, session: AuthoringSession | null): AuthoringSession | null {
  return workspace && session?.owner.workspace.workspace_id === workspace.id ? session : null;
}

export interface PackageValidation {
  state: 'none' | 'running' | 'valid' | 'invalid' | 'stale';
  revision: string | null;
  count: number;
  unsaved: number;
}

export function packageValidation(session: AuthoringSession, validating: boolean): PackageValidation {
  return {
    state: validating || session.pending?.kind === 'validate' ? 'running'
      : session.validation === null ? 'none' : !validationCurrent(session) ? 'stale'
      : session.validation.valid ? 'valid' : 'invalid',
    revision: session.validation?.revision ?? null,
    count: session.validation?.diagnostics.length ?? 0,
    unsaved: dirtyDrafts(session).length,
  };
}

export interface AuthoringActivity {
  owner: AuthoringRef;
  packageId: string | null;
  worker: {kind: string; phase: string; run: string | null; cancellable: boolean} | null;
  capture: NativeSelectionView | null;
  trial: RecognitionTrial | null;
  cleanupIncomplete: boolean;
  stopRequested: boolean;
  items: {id: string; severity: 'error' | 'warning'; text: string; recognition: boolean}[];
}

// Global Edit facts never borrow the selected Tab's notices. Terminal controller snapshots have no lease token:
// use the session's retained outcomes instead, so replacing a lease cannot revive its predecessor's details.
export function authoringActivity(session: AuthoringSession | null, hostOwner: AuthoringRef | null | undefined,
  operation: ControllerView, native: NativeSelectionView | null, locale: Locale, leaseLost = false): AuthoringActivity | null {
  const owner = session?.owner ?? hostOwner;
  if (!owner) return null;
  const ui = messages[locale].ui;
  const t = messages[locale].app;
  const recognition = session?.recognition && sameAuthoringRef(session.recognition.owner, owner) ? session.recognition : null;
  const capture = native && sameAuthoringRef(native.owner, owner) ? native : null;
  const live = busy(operation.state) && operation.workspace_id === owner.workspace.workspace_id
    && operation.workspace_revision === owner.workspace.revision
    && (operation.operation === 'authoring_validate' || operation.operation.startsWith('recognition_'));
  const pending = session?.pending?.kind;
  const trialPending = pending === 'recognition_trial' || recognition?.running != null;
  const worker = live ? {kind: ui.operation(operation.operation), phase: operation.state, run: operation.run, cancellable: true}
    : pending === 'validate' || trialPending
      ? {kind: ui.operation(pending === 'validate' ? 'authoring_validate' : recognition?.running ? 'recognition_trial' : 'recognition_capabilities'),
        phase: 'preparing', run: null, cancellable: true}
      : pending ? {kind: {save: t.savingFile, catalog: t.changingCatalog, refresh: t.refreshingPackage, validate: ui.authoring.validating,
        recognition: t.updatingRecognition, recognition_trial: ui.operation('recognition_trial'), exit: t.exitingEdit, duplicate: t.duplicatingPackage}[pending],
        phase: 'preparing', run: null, cancellable: false} : null;
  const items: AuthoringActivity['items'] = [];
  const fault = (id: string, value: Fault | null | undefined, recognition = false) => {
    if (value) items.push({id, severity: 'error', text: value instanceof LocalFault
      ? renderMessage(locale, value.presentation) : faultSummary(value), recognition});
  };
  const retainedTrial = recognition?.trial?.trial;
  const trial = retainedTrial && sameAuthoringRef(retainedTrial.owner, owner) ? retainedTrial : null;
  const cleanupIncomplete = trial !== null && trial.controller.result !== null
    && (record(trial.controller.result.cleanup).clean !== true || trial.controller.result.forced === true);
  if (trial && (trial.controller.error || trial.controller.result === null || trial.controller.result.primary != null || cleanupIncomplete)) {
    const view = trial.controller;
    const primary = view.error ?? record(view.result?.primary);
    items.push({id: 'trial-outcome', severity: 'error', recognition: true,
      text: `${ui.operation('recognition_trial')} · ${Object.keys(primary).length ? faultSummary(primary) : t.unresolved} · ${ui.common.cleanup}: ${cleanupLabel(view.result, record(view.error?.context), locale)}`});
  }
  fault('trial-refused', recognition?.trial?.fault, true);
  fault('capture-error', capture?.error, true);
  fault('edit-error', session?.error);
  fault('edit-refresh-error', session?.refreshError);
  fault('recognition-error', recognition?.error, true);
  if (leaseLost) items.push({id: 'lease-lost', severity: 'error', text: ui.authoring.leaseLostShort, recognition: false});
  if (session?.refreshRequired) items.push({id: 'edit-refresh', severity: 'warning', text: ui.authoring.refreshRequired, recognition: false});
  if (session?.conflict) items.push({id: 'edit-conflict', severity: 'warning', text: ui.authoring.conflictHeading, recognition: false});
  return {owner, packageId: session?.packageId ?? null, worker, capture, trial, cleanupIncomplete, items,
    stopRequested: (worker?.cancellable === true || capture?.busy === true) && session?.notice?.key === 'authoringStopRequested'};
}

export function applicationStatus(operation: ControllerView, locale: Locale, pending: string | null = null, starting = false): StatusItem[] {
  const ui = messages[locale].ui;
  const items: StatusItem[] = [];
  if (pending) items.push({id: 'command', severity: 'progress', text: pending, action: null});
  if (starting || (operation.workspace_id === null && busy(operation.state))) {
    items.push({id: 'operation', severity: 'progress',
      text: `${ui.operation(starting ? 'environment_check' : operation.operation)} · ${ui.phase(starting ? 'preparing' : operation.state)}`, action: null});
  }
  return items;
}

// The ordered list is a projection, not a last-message store. Private operation text stays in Run details.
export function workspaceStatus(workspace: Workspace, facts: Derived | undefined, operation: ControllerView | null,
  authoring: AuthoringSession | null, locale: Locale, starting = false, leaseLost = false): StatusItem[] {
  const t = messages[locale].app;
  const ui = messages[locale].ui;
  const session = scopedAuthoring(workspace, authoring);
  const items: StatusItem[] = [];
  const add = (id: string, severity: StatusItem['severity'], value: string | null, action: StatusItem['action'], earlierDraft?: boolean) => {
    if (value) items.push({id, severity, text: value, action, earlierDraft});
  };
  const fault = (id: string, value: Fault | null | undefined, action: StatusItem['action'], earlierDraft?: boolean) => {
    if (value) add(id, 'error', value instanceof LocalFault ? renderMessage(locale, value.presentation) : `${value.category} · ${value.message}`, action, earlierDraft);
  };
  const owned = operation?.workspace_id === workspace.id ? operation : null;
  if (needsAttention(owned) && owned) {
    const primary = owned.error ?? record(owned.result?.primary);
    add('execution', 'error', `${ui.run.immutable} · ${owned.error || Object.keys(primary).length ? faultSummary(primary) : text(owned.result?.status) ?? t.unresolved} · ${ui.common.cleanup}: ${cleanupLabel(owned.result, record(owned.error?.context), locale)}`, 'execution');
  }
  fault('workspace-error', workspace.error, 'run');
  fault('source-error', workspace.sourceError, 'run');
  fault('profiles-error', workspace.bound?.profilesError, 'run');
  const target = workspace.bound?.target;
  fault('target-read', target?.readError, 'run');
  fault('target-action', target?.issue?.fault, 'run', target?.issue ? !currentTargetDraft(target, target.issue.ticket) : undefined);
  add('target-conflict', 'warning', target?.reconcile ? ui.target.conflict : null, 'run');
  if (session) {
    fault('edit-error', session.error, 'edit');
    fault('edit-refresh-error', session.refreshError, 'edit');
    add('lease-lost', 'error', leaseLost ? ui.authoring.leaseLost : null, 'edit');
    add('edit-refresh', 'warning', session.refreshRequired ? ui.authoring.refreshRequired : null, 'edit');
    add('edit-conflict', 'warning', session.conflict ? ui.authoring.conflictHelp : null, 'edit');
  } else if (workspace.bound === null) {
    add('inspection', 'warning', ui.status.inspectionRequired, 'run');
  }
  add('recovery', 'warning', workspace.recovery?.view.binding_required ? t.bindingRequired : null, 'run');
  add('fields', 'warning', facts?.numericErrors ? t.invalidFields : null, 'run');
  add('profile', 'warning', facts && !facts.bound ? t.staleProfile : null, 'run');
  add('descriptor', 'warning', facts?.descriptorError ?? null, 'run');
  add('start-block', 'warning', facts?.startBlock ?? null, 'run');
  add('command', 'progress', renderMessage(locale, workspace.busy), null);
  if (starting || (owned && busy(owned.state))) add('operation', 'progress', ui.phase(starting ? 'preparing' : owned!.state), 'execution');
  add('notice', 'info', renderMessage(locale, session?.notice ?? workspace.notice), null);
  return items;
}

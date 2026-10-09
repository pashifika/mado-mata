import {dirtyDrafts, validationCurrent} from './authoring.ts';
import type {AuthoringSession} from './authoring.ts';
import {LocalFault, messages, renderMessage} from './i18n.ts';
import type {Locale} from './i18n.ts';
import {cleanupLabel, faultSummary, record, text} from './state.ts';
import type {ControllerView, Fault} from './types.ts';
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

export function packageValidation(session: AuthoringSession, validating: boolean) {
  return {
    state: validating || session.pending?.kind === 'validate' ? 'running' as const
      : session.validation === null ? 'none' as const : !validationCurrent(session) ? 'stale' as const
      : session.validation.valid ? 'valid' as const : 'invalid' as const,
    revision: session.validation?.revision ?? null,
    count: session.validation?.diagnostics.length ?? 0,
    unsaved: dirtyDrafts(session).length,
  };
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

// The ordered list is a projection, not a last-message store. Private operation text stays in Execution.
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
  if (session?.pending) {
    const progress = {save: t.savingFile, catalog: t.changingCatalog, refresh: t.refreshingPackage, validate: ui.authoring.validating,
      recognition: t.updatingRecognition, recognition_trial: ui.operation('recognition_trial'), exit: t.exitingEdit, duplicate: t.duplicatingPackage};
    add('edit-command', 'progress', progress[session.pending.kind], 'edit');
  }
  add('notice', 'info', renderMessage(locale, session?.notice ?? workspace.notice), null);
  return items;
}

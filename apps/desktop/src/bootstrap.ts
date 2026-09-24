import type {Locale} from './i18n.ts';
import {DEFAULT_NOTIFICATIONS, record} from './state.ts';
import type {BootstrapStatus, EditableSettings, Fault, Json, SnapshotReceipt} from './types.ts';

export type BootstrapAction = 'initializing' | 'retrying' | 'importingRoot' | 'restoring' | 'recovering' | 'refreshingStatus';

// Setup's saved-language draft is an explicit choice written by Initialize; it is not the temporary presentation.
export interface SetupDraft {locale:Locale; backupDirectory:string; startFresh:boolean}
// Each reconstructing action carries its own session-disposal consent; ticking one never pre-consents another.
export interface RestoreDraft {archivePath:string; confirm:boolean; discard:boolean; recoverConfirm:boolean; recoverDiscard:boolean; retryDiscard:boolean}

// A dispatched snapshot keeps its real outcome after the requesting view is gone.
export type SnapshotOutcome = {kind:'receipt'; receipt:SnapshotReceipt} | {kind:'fault'; fault:Fault};

export interface BootstrapUi {
  status:BootstrapStatus|null;
  // Temporary English/日本語 presentation before trusted settings exist; never persisted or salvaged from bytes.
  presentation:Locale;
  setup:SetupDraft;
  restore:RestoreDraft;
  // Per-action destination override; blank means the host's saved or default destination.
  snapshotDestination:string;
  pending:BootstrapAction|null;
  // Transport failure of the latest action. `status.fault` is the authoritative cause and is never cleared here.
  actionError:Fault|null;
  snapshotPending:boolean;
  snapshotOutcome:SnapshotOutcome|null;
  receiptGeneration:string|null;
}

export const INITIAL_BOOTSTRAP: BootstrapUi = {
  status: null, presentation: 'en',
  setup: {locale: 'en', backupDirectory: '', startFresh: false},
  restore: {archivePath: '', confirm: false, discard: false, recoverConfirm: false, recoverDiscard: false, retryDiscard: false},
  snapshotDestination: '', pending: null, actionError: null, snapshotPending: false, snapshotOutcome: null, receiptGeneration: null,
};

export type BootstrapEvent =
  | {type:'status'; status:BootstrapStatus}
  | {type:'presentation'; locale:Locale}
  | {type:'setup'; draft:SetupDraft}
  | {type:'restore'; draft:RestoreDraft}
  | {type:'snapshotDestination'; value:string}
  | {type:'pending'; action:BootstrapAction|null}
  | {type:'actionFailed'; fault:Fault}
  | {type:'dismissActionError'}
  | {type:'snapshotPending'}
  | {type:'snapshotSettled'; outcome:SnapshotOutcome}
  | {type:'receiptConsumed'};

// Form edits, dismissals and failed actions never touch the host status; only a host status replaces it.
export function reduceBootstrap(ui:BootstrapUi, event:BootstrapEvent):BootstrapUi {
  switch (event.type) {
    case 'status': return {...ui, status: event.status};
    case 'presentation': return {...ui, presentation: event.locale};
    case 'setup': return {...ui, setup: event.draft};
    case 'restore': return {...ui, restore: event.draft};
    case 'snapshotDestination': return {...ui, snapshotDestination: event.value};
    case 'pending': {
      const settled = event.action === null && ui.pending !== null && ui.pending !== 'refreshingStatus';
      return {...ui, pending:event.action, actionError:event.action === null ? ui.actionError : null,
        restore:settled ? {...ui.restore, confirm:false, discard:false, recoverConfirm:false, recoverDiscard:false, retryDiscard:false} : ui.restore};
    }
    case 'actionFailed': return {...ui, actionError: event.fault};
    case 'dismissActionError': return {...ui, actionError: null};
    case 'snapshotPending': return {...ui, snapshotPending: true};
    case 'snapshotSettled': return {...ui, snapshotPending: false, snapshotOutcome: event.outcome,
      receiptGeneration:event.outcome.kind === 'receipt' ? event.outcome.receipt.generation : ui.receiptGeneration};
    case 'receiptConsumed': return {...ui, receiptGeneration:null};
  }
}

export type Surface = 'loading' | 'setup' | 'recovery' | 'shell';

// Only a usable Application admits the normal shell; a later fault keeps the shell and shows Recovery inside it.
// The host's transient `loading` state is not Setup: no defaults or Initialize are offered until it resolves.
export function surface(status:BootstrapStatus|null):Surface {
  if (status === null || status.state === 'loading') return 'loading';
  if (status.application_available) return 'shell';
  return status.state === 'setup' ? 'setup' : 'recovery';
}

// Initialize writes exactly the operator's explicit choices plus documented defaults; nothing is inspected or probed.
export function initialSettings(draft:SetupDraft):EditableSettings {
  const destination = draft.backupDirectory.trim();
  return {locale: draft.locale, gui_log_limit: 1000, ocr_environment: null, notifications: {...DEFAULT_NOTIFICATIONS}, backup_directory: destination === '' ? null : destination};
}

// Only a constructing action that ended Ready without a fault can have rebuilt the Application; its catalog then
// carries fresh session IDs. A refused or failed action, a transiently absent catalog and a plain status refresh keep
// the current session and its drafts untouched.
export function reconstructed(status:BootstrapStatus, current:readonly {id:string}[]):boolean {
  const catalog = status.catalog;
  if (catalog === null || status.state !== 'ready' || status.fault !== null) return false;
  return !catalog.open.some(view => current.some(workspace => workspace.id === view.workspace_id));
}

export type RestoreOutcome =
  | 'unfinished' | 'rollbackFailed' | 'rolledBackAutomatically'
  | 'installedNotReconstructed' | 'rolledBackNotReconstructed'
  | 'installedCleanupPending' | 'rolledBackCleanupPending'
  | 'installedCleanupUnconfirmed' | 'rolledBackCleanupUnconfirmed';

type Direction = 'installed' | 'rolledBack';

// The generation a transaction verified before its cleanup: `rolled_back` refines `configuration_installed`.
function direction(context:Record<string,Json>):Direction|null {
  if (context.rolled_back === true) return 'rolledBack';
  return context.configuration_installed === true ? 'installed' : null;
}

// Cleanup of the requested direction reports at the top level; cleanup after an automatic rollback nests its own
// fault under `rollback_cleanup`.
function cleanupDirection(context:Record<string,Json>):Direction|null {
  if (context.cleanup_incomplete === true) return direction(context);
  const nested = record(record(context.rollback_cleanup).context);
  return nested.cleanup_incomplete === true ? direction(nested) : null;
}

// Recovery reached through a restore says what the managed configuration set now holds. The host's `pending_restore`
// alone decides whether cleanup must still be finished with the recovery controls or Retry is offered. A replacement
// without a verified generation (failed automatic rollback, interrupted or failed recovery, or an unrecognized
// transaction diagnostic) never reads as untouched data. Only boolean flags select a verified outcome; a nested fault
// counts by presence. No transaction signal reports nothing.
export function restoreOutcome(status:BootstrapStatus):RestoreOutcome|null {
  const context = record(status.fault?.context);
  const cleanup = cleanupDirection(context);
  if (cleanup === 'installed') return status.pending_restore ? 'installedCleanupPending' : 'installedCleanupUnconfirmed';
  if (cleanup === 'rolledBack') return status.pending_restore ? 'rolledBackCleanupPending' : 'rolledBackCleanupUnconfirmed';
  if (context.rollback_failure !== undefined) return 'rollbackFailed';
  if (status.pending_restore || context.pending_restore === true || context.cleanup_incomplete === true
    || context.rollback_cleanup !== undefined) return 'unfinished';
  if (context.configuration_installed === true) return context.rolled_back === true ? 'rolledBackNotReconstructed' : 'installedNotReconstructed';
  return context.rolled_back === true ? 'rolledBackAutomatically' : null;
}

// Serializes polling against session reconstruction. A constructing action holds the gate synchronously, so no new
// poll is dispatched once the host may publish a rebuilt Application, and waits for the poll already in flight to
// finish ingesting into the old session. Polls are delayed, never dropped; the hold ends when the action settles.
export class PollGate {
  private holds = 0;
  private inflight:Promise<void>|null = null;
  private settle:(() => void)|null = null;

  // The poll loop claims the gate before each dispatch; a refused claim means try again on the next tick.
  // Executor form: the project's ES2022 lib has no `Promise.withResolvers` typing.
  claim():boolean {
    if (this.holds > 0 || this.inflight !== null) return false;
    this.inflight = new Promise(resolve => {this.settle = resolve;});
    return true;
  }

  // Called once the response is ingested or the poll failed, whichever ends the claim.
  release():void {
    const settle = this.settle;
    this.inflight = null;
    this.settle = null;
    settle?.();
  }

  // Pauses new dispatch immediately; the returned promise resolves once the in-flight poll has released.
  hold():Promise<void> {
    this.holds += 1;
    return this.inflight ?? Promise.resolve();
  }

  resume():void {
    this.holds -= 1;
  }
}

export interface Admission {active:boolean; command:boolean; anyDirty:boolean}
export type RestoreBlock = 'pendingRestore' | 'active' | 'command' | 'archivePath' | 'confirm' | 'discard';

// Restore needs an idle, settled Application and explicit scope/discard confirmation before the host is asked.
export function restoreBlock(status:BootstrapStatus, draft:RestoreDraft, admission:Admission):RestoreBlock|null {
  if (status.pending_restore) return 'pendingRestore';
  if (admission.active) return 'active';
  if (admission.command) return 'command';
  if (draft.archivePath.trim() === '') return 'archivePath';
  if (!draft.confirm) return 'confirm';
  if ((status.application_available || admission.anyDirty) && !draft.discard) return 'discard';
  return null;
}

export type ReconstructionBlock = 'active' | 'command' | 'discard';

// Every retained Application requires explicit session disposal, even with no dirty profile.
export function reconstructionBlock(status:BootstrapStatus, admission:Admission, discard:boolean):ReconstructionBlock|null {
  if (admission.active) return 'active';
  if (admission.command) return 'command';
  if ((status.application_available || admission.anyDirty) && !discard) return 'discard';
  return null;
}

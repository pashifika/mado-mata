import {sameAuthoringRef} from './authoring.ts';
import type {AuthoringController, ExportTicket} from './authoring-controller.ts';
import type {SaveSequence} from './authoring-publication.ts';
import type {AuthoringRef, Fault, WorkspaceRef} from './types.ts';

// Public facts of one captured signed release. The signature is valid relative to `key_id` only; nothing here verifies
// an account, a publisher or authorship. `revision` is the captured saved revision, `artifact_id` the archive SHA-256.
export interface ReleaseFacts {
  package_id:string; release_version:string; name:string; description:string; game_names:Record<string,string>;
  key_id:string; revision:string; artifact_id:string; files:number; bytes:number;
}
// A host review of exactly one captured artifact, bound to its originating Workspace (and Edit owner for export).
// Only `token` authorizes publication of that capture; `account_verified` is always false.
export interface DistributionReview extends ReleaseFacts {
  token:string; workspace:WorkspaceRef; owner:AuthoringRef|null; destination:string; runtime:string;
  signature_valid:true; account_verified:false;
}
// The durable outcome of a publication. Refusals arrive as faults, never as a Publication. `unchanged` reused a verified
// identical installation; `cleanup_error` accompanies a committed artifact whose owned staging could not be removed.
export interface Publication {
  path:string; package_id:string; release_version:string; artifact_id:string; unchanged:boolean; committed:boolean; cleanup_error:Fault|null;
}
// One installed release archive, reverified per listing: exactly one of `facts` (verified) and `fault` (attributable) is set.
export interface InstalledRelease {package_id:string; artifact_id:string; facts:ReleaseFacts|null; fault:Fault|null}
// Public local signer facts. `display_label` is a cached local account label, not a verified publisher identity.
export interface SigningInfo {key_id:string; display_label:string; enrolled_at:number; provider:string; local_only:boolean}

// Signer status as last read: `info` is undefined until a read succeeds and null when no key exists; an unusable key is
// `statusError`, never "not enrolled". Enrollment and its cancellation report their own outcomes.
export interface SignerState {
  info:SigningInfo|null|undefined; statusError:Fault|null; enrollError:Fault|null; cancelError:Fault|null; enrolling:boolean; cancelling:boolean;
}

// Shown on the ready step: the destination panel closed without a choice, or the previous attempt was refused.
export type ExportNote = {kind:'unchosen'} | {kind:'failed'; stage:'prepare'|'publish'; error:Fault};
export type ExportStep =
  // Contributing drafts are unsaved: Save and continue, or Cancel with every draft intact.
  | {kind:'drafts'}
  | {kind:'saving'}
  // Save did not leave a clean saved revision; committed saves and remaining drafts stay as they are.
  | {kind:'stopped'; sequence:SaveSequence; remaining:boolean}
  | {kind:'ready'; note:ExportNote|null}
  | {kind:'choosing'}
  | {kind:'preparing'; attempt:number; ticket:ExportTicket; destination:string; cancelling:boolean; cancelError:Fault|null}
  | {kind:'review'; ticket:ExportTicket; review:DistributionReview}
  | {kind:'publishing'; ticket:ExportTicket; review:DistributionReview}
  | {kind:'published'; review:DistributionReview; publication:Publication};
export interface ExportFlow {owner:AuthoringRef; step:ExportStep}

export type ImportStep =
  | {kind:'choosing'}
  | {kind:'preparing'; attempt:number; path:string; cancelling:boolean; cancelError:Fault|null}
  | {kind:'review'; review:DistributionReview}
  | {kind:'publishing'; review:DistributionReview}
  | {kind:'published'; review:DistributionReview; publication:Publication}
  | {kind:'failed'; error:Fault};
export interface ImportFlow {workspace:WorkspaceRef; step:ImportStep}

export type PublicationOutcome = 'clean'|'unchanged'|'cleanup'|'uncommitted'|'mismatch';

export function sameWorkspace(a:WorkspaceRef, b:WorkspaceRef):boolean {
  return a.workspace_id === b.workspace_id && a.revision === b.revision;
}

// The release version the saved manifest declares, for display and the suggested file name only; the host validates.
export function declaredRelease(manifest:string|null):string|null {
  if (manifest === null) return null;
  try {
    const release = JSON.parse(manifest)?.distribution?.release_version;
    return typeof release === 'string' ? release : null;
  } catch {
    return null;
  }
}

export function suggestedName(packageId:string, release:string|null):string {
  const name = `${release === null ? packageId : `${packageId}-${release}`}.mmpkg`;
  return new TextEncoder().encode(name).length <= 255 ? name : `${packageId}.mmpkg`;
}

// Where export stands for `owner` now, after an explicit Save and continue (`sequence`) or before a new capture. A
// clean, unchanged saved revision is ready; a Save failure or a draft still unsaved (for example a later draft that a
// delayed Save reply kept dirty) stops before preparation with the actual committed and remaining facts.
export function exportReadiness(controller:AuthoringController, owner:AuthoringRef, sequence:SaveSequence|null):ExportStep {
  if (sequence !== null && (sequence.failure !== null || sequence.remaining.length > 0)) {
    return {kind: 'stopped', sequence, remaining: sequence.remaining.length > 0};
  }
  const ticket = controller.exportTicket();
  if (ticket !== null && sameAuthoringRef(ticket.owner, owner)) return {kind: 'ready', note: null};
  const session = controller.current();
  const unsaved = session !== null && sameAuthoringRef(session.owner, owner)
    && controller.describe().resources.some(resource => resource.dirty && (resource.role === undefined || resource.role === 'context'));
  if (unsaved) return sequence === null ? {kind: 'drafts'} : {kind: 'stopped', sequence, remaining: true};
  return {kind: 'ready', note: {kind: 'failed', stage: 'prepare', error: {category: 'AuthoringExportUnavailable',
    message: 'The Edit owner changed, or its saved revision awaits a refresh, conflict resolution or another command', context: null}}};
}

// Applies a preparation reply only to the attempt still waiting for it. A review for another owner or saved revision
// than the captured ticket is refused rather than shown as that capture.
export function acceptExportReview(flow:ExportFlow|null, attempt:number, review:DistributionReview):ExportFlow|null {
  const step = flow?.step;
  if (!flow || step?.kind !== 'preparing' || step.attempt !== attempt) return flow;
  if (step.cancelling) return failExportPreparation(flow, attempt, {category: 'PackageCancelled', message: 'Package preparation was cancelled', context: null});
  if (!review.owner || !sameAuthoringRef(review.owner, step.ticket.owner) || !sameWorkspace(review.workspace, step.ticket.owner.workspace)
    || review.revision !== step.ticket.revision) {
    return {...flow, step: {kind: 'ready', note: {kind: 'failed', stage: 'prepare', error: {category: 'DistributionReviewMismatch',
      message: 'The prepared review does not match the requested owner or saved revision', context: null}}}};
  }
  return {...flow, step: {kind: 'review', ticket: step.ticket, review}};
}

export function failExportPreparation(flow:ExportFlow|null, attempt:number, error:Fault):ExportFlow|null {
  const step = flow?.step;
  return flow && step?.kind === 'preparing' && step.attempt === attempt ? {...flow, step: {kind: 'ready', note: {kind: 'failed', stage: 'prepare', error}}} : flow;
}

// Confirmation is guarded synchronously at the moment it is requested: the same owner, saved revision and contributing
// draft versions as the reviewed capture, or nothing is sent.
export function exportConfirmable(flow:ExportFlow|null, controller:AuthoringController):flow is ExportFlow & {step:Extract<ExportStep, {kind:'review'}>} {
  return flow !== null && flow.step.kind === 'review' && controller.exportCurrent(flow.step.ticket);
}

export function acceptImportReview(flow:ImportFlow|null, attempt:number, review:DistributionReview):ImportFlow|null {
  const step = flow?.step;
  if (!flow || step?.kind !== 'preparing' || step.attempt !== attempt) return flow;
  if (step.cancelling) return failImportPreparation(flow, attempt, {category: 'PackageCancelled', message: 'Package preparation was cancelled', context: null});
  if (review.owner !== null || !sameWorkspace(review.workspace, flow.workspace)) {
    return {...flow, step: {kind: 'failed', error: {category: 'DistributionReviewMismatch', message: 'The verified review names another Workspace owner', context: null}}};
  }
  return {...flow, step: {kind: 'review', review}};
}

export function failImportPreparation(flow:ImportFlow|null, attempt:number, error:Fault):ImportFlow|null {
  const step = flow?.step;
  return flow && step?.kind === 'preparing' && step.attempt === attempt ? {...flow, step: {kind: 'failed', error}} : flow;
}

// A receipt counts only for the reviewed artifact; a committed artifact with failed cleanup is never clean success.
export function publicationOutcome(review:ReleaseFacts, publication:Publication):PublicationOutcome {
  if (publication.package_id !== review.package_id || publication.release_version !== review.release_version
    || publication.artifact_id !== review.artifact_id) return 'mismatch';
  if (publication.cleanup_error !== null) return 'cleanup';
  if (publication.unchanged) return 'unchanged';
  return publication.committed ? 'clean' : 'uncommitted';
}

export function shortDigest(value:string):string {
  return value.length > 16 ? `${value.slice(0, 16)}…` : value;
}

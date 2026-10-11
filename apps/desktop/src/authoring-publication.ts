import {applySave, beginPending, failCommand, saveBlock, saveTicket} from './authoring.ts';
import type {SaveTicket} from './authoring.ts';
import type {AuthoringController} from './authoring-controller.ts';
import type {AuthoringMutation, AuthoringRef, Fault} from './types.ts';

export interface SaveTarget {resource:string; version:string}
export interface SaveOutcome {
  path:string; committedRevision:string|null; refreshRequired:boolean; error:Fault|null; outcome?:'unknown';
}
export interface SaveSequence {
  committed:{path:string; revision:string}[]; remaining:SaveTarget[]; failure:SaveOutcome|null;
}
export interface PublicationPorts {
  save(owner:AuthoringRef, ticket:SaveTicket):Promise<AuthoringMutation>;
  refreshRecognition():Promise<boolean>;
  fault(cause:unknown):Fault;
  available(owner:AuthoringRef):boolean;
}

function refused(path:string, category:string, message:string):SaveOutcome {
  return {path, committedRevision:null, refreshRequired:false, error:{category, message, context:{path}}};
}

// A receipt records the actual publication, not a later read of the global revision.
export async function saveFileDraft(controller:AuthoringController, path:string, ports:PublicationPorts,
  expected?:SaveTarget):Promise<SaveOutcome> {
  const session = controller.current();
  if (!session || !ports.available(session.owner)) return refused(path, 'AuthoringOwner', 'The Edit owner is unavailable');
  if (expected) {
    const resource = controller.resource(expected.resource);
    if (!resource || resource.path !== path || resource.version !== expected.version) {
      return refused(path, 'AuthoringDraftChanged', 'The requested draft version changed before Save');
    }
  }
  if (session.drafts.get(path)?.composing !== null && session.drafts.get(path)?.composing !== undefined) {
    return refused(path, 'AuthoringComposing', 'Text input is being composed in this file');
  }
  const block = saveBlock(session, path);
  if (block === 'clean') return {path, committedRevision:null, refreshRequired:false, error:null};
  if (block !== null) return refused(path, `AuthoringSaveBlocked_${block}`, `Save is unavailable: ${block}`);
  const ticket = saveTicket(session, path)!;
  controller.update(current => current?.owner.token === ticket.token ? beginPending(current, {kind:'save', path}) : current);
  let mutation:AuthoringMutation;
  try {
    mutation = await ports.save(session.owner, ticket);
  } catch (cause) {
    const error = ports.fault(cause);
    controller.update(current => failCommand(current, ticket.token, error));
    return {path, committedRevision:null, refreshRequired:false, error, outcome:'unknown'};
  }
  controller.update(current => applySave(current, ticket, mutation));
  const current = controller.current();
  const result:SaveOutcome = {path, committedRevision:mutation.committed_revision,
    refreshRequired:current?.owner.token === ticket.token ? current.refreshRequired
      : mutation.view === null || mutation.view.owner.token !== ticket.token, error:mutation.refresh_error};
  if (!current || current.owner.token !== ticket.token) return result;
  if (session.recognition && mutation.view !== null && !await ports.refreshRecognition()) {
    result.error = controller.current()?.error ?? {category:'RecognitionUnavailable', message:'Saved bytes committed, but recognition context could not refresh', context:null};
  }
  return result;
}

// Each step captures the latest saved revision but may only publish its originally named draft version.
export async function saveFiles(controller:AuthoringController, targets:readonly SaveTarget[], ports:PublicationPorts):Promise<SaveSequence> {
  const owner = controller.current()?.owner.token;
  const committed:SaveSequence['committed'] = [];
  for (let index = 0; index < targets.length; index += 1) {
    const target = targets[index];
    const resource = controller.resource(target.resource);
    const outcome = controller.current()?.owner.token !== owner || !resource
      ? refused(resource?.path ?? '', 'AuthoringOwner', 'The Edit owner or resource changed during Save all')
      : await saveFileDraft(controller, resource.path, ports, target);
    if (outcome.committedRevision !== null) committed.push({path:outcome.path, revision:outcome.committedRevision});
    if (outcome.error !== null || outcome.refreshRequired) {
      return {committed, remaining:targets.slice(outcome.committedRevision === null ? index : index + 1), failure:outcome};
    }
  }
  return {committed, remaining:[], failure:null};
}

import {ENVIRONMENT_LANGUAGE, ENVIRONMENT_PROVIDER, ENVIRONMENT_RUNTIME_PROFILE, SUPPORTED_PROFILES, environmentDraft} from '../state.ts';
import type {SettingsDraft} from '../state.ts';
import type {Fault, OcrEnvironment} from '../types.ts';

export interface LocalizedText {en:string; ja:string}
export interface SetupLink {label:LocalizedText; url:string}
export interface SetupItem {
  id:string; name:LocalizedText; state:string; detail:LocalizedText;
  downloadable:boolean; links:SetupLink[]; commands:string[];
}
export interface SetupView {items:SetupItem[]; environment:OcrEnvironment|null}
export interface SetupProgress {stage:string; resource_id:string; bytes:number; total:number}
export interface SetupOperation {
  id:string; active:boolean; progress:SetupProgress; error:Fault|null; cleanup_error:Fault|null; result:SetupView|null;
}
export interface SetupClient {
  catalog:() => Promise<SetupView>;
  start:(resourceId:string, environment:OcrEnvironment|null) => Promise<string>;
  poll:() => Promise<SetupOperation|null>;
  cancel:(operationId:string) => Promise<void>;
  openLink:(resourceId:string, index:number) => Promise<void>;
  copyCommand:(resourceId:string, index:number) => Promise<void>;
}
export interface SetupState {
  view:SetupView|null; catalogPending:boolean; starting:string|null; resourceId:string|null; id:string|null; operation:SetupOperation|null;
  owned:boolean; cancelRequested:boolean; cancelPending:boolean; error:unknown; pollError:unknown; unavailable:boolean;
  guidancePending:string|null; copied:string|null; checkedRevision:number|null; applied:boolean;
}
export const INITIAL_SETUP:SetupState = {
  view:null, catalogPending:false, starting:null, resourceId:null, id:null, operation:null,
  owned:false, cancelRequested:false, cancelPending:false, error:null, pollError:null, unavailable:false,
  guidancePending:null, copied:null, checkedRevision:null, applied:false,
};

// Recheck accepts partial path hints; settings Save still requires readEnvironment's complete tuple.
export function setupEnvironment(draft:SettingsDraft['environment']):OcrEnvironment|null {
  const libraries = draft.library_paths.split(/\r?\n/).map(path => path.trim()).filter(Boolean);
  const modelRoot = draft.model_root.trim();
  const runtimePath = draft.runtime_path.trim();
  if (!draft.profile && !modelRoot && !runtimePath && !libraries.length) return null;
  return {
    profile:draft.profile, model:SUPPORTED_PROFILES.find(item => item.profile === draft.profile)?.model ?? draft.profile,
    language:ENVIRONMENT_LANGUAGE, provider:ENVIRONMENT_PROVIDER, runtime_profile:ENVIRONMENT_RUNTIME_PROFILE,
    model_root:modelRoot, runtime_path:runtimePath, native_library_paths:libraries,
  };
}

// One dialog owns this session; neither category selection nor a successor dialog can adopt its job.
export class OcrSetupSession {
  private closed = false;
  private state:SetupState = {...INITIAL_SETUP};
  private draftKey:string;
  private revision = 0;
  private inspectionRevision:number|null = null;
  private polling = false;
  private client:SetupClient;
  private publish:(state:SetupState) => void;

  constructor(client:SetupClient, draft:SettingsDraft, publish:(state:SetupState) => void) {
    this.client = client;
    this.publish = publish;
    this.draftKey = JSON.stringify(draft);
  }

  get snapshot():SetupState {return this.state;}
  get busy():boolean {return this.state.cancelPending || this.state.starting !== null || Boolean(this.state.operation?.cleanup_error) || (this.state.id !== null && this.state.operation?.active !== false);}
  get draftRevision():number {return this.revision;}

  observeDraft(draft:SettingsDraft):void {
    const key = JSON.stringify(draft);
    if (key !== this.draftKey) {this.draftKey = key; this.revision += 1;}
  }

  private update(next:Partial<SetupState>):void {
    if (this.closed) return;
    this.state = {...this.state, ...next};
    this.publish(this.state);
  }

  async load():Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending) return;
    this.update({catalogPending:true, error:null});
    try {
      const view = await this.client.catalog();
      this.update({view});
    } catch (error) {this.update({catalogPending:false, error}); return;}
    if (this.closed) return;
    try {
      const operation = await this.client.poll();
      this.update({operation, owned:false, checkedRevision:null, id:operation?.id ?? null, resourceId:operation?.progress.resource_id ?? null, pollError:null});
    } catch (pollError) {this.update({pollError});}
    finally {this.update({catalogPending:false});}
  }

  async start(resourceId:string, draft:SettingsDraft):Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending) return;
    this.observeDraft(draft);
    this.inspectionRevision = resourceId === 'inspect' ? this.revision : null;
    this.update({starting:resourceId, resourceId, owned:true, id:null, operation:null, checkedRevision:null, applied:false,
      view:this.state.view ? {...this.state.view, environment:null} : null,
      cancelRequested:false, cancelPending:false, error:null, pollError:null, unavailable:false});
    try {
      const id = await this.client.start(resourceId, resourceId === 'inspect' ? setupEnvironment(draft.environment) : null);
      if (this.closed) {
        // Start may return after close; cancel that exact ID, never the host's current job.
        try {await this.client.cancel(id);} catch { /* No live dialog remains; backend admission retains ownership. */ }
        return;
      }
      this.update({id, starting:null});
      if (this.state.cancelRequested) await this.cancel();
    } catch (error) {this.update({starting:null, owned:false, error});}
  }

  async poll():Promise<void> {
    const id = this.state.id;
    if (this.closed || id === null || !this.busy || this.polling) return;
    this.polling = true;
    try {
      const operation = await this.client.poll();
      if (this.closed || this.state.id !== id) return;
      if (!this.state.owned) {
        this.update({operation, id:operation?.id ?? null, resourceId:operation?.progress.resource_id ?? null, pollError:null, unavailable:false});
        return;
      }
      if (operation === null || operation.id !== id) {
        this.update({unavailable:true});
        return;
      }
      const next:Partial<SetupState> = {operation, pollError:null, unavailable:false};
      if (!operation.active && operation.progress.stage === 'complete' && !operation.error && !operation.cleanup_error
        && this.state.resourceId === 'inspect' && operation.result !== null) {
        next.view = operation.result;
        next.checkedRevision = this.inspectionRevision;
      }
      this.update(next);
    } catch (pollError) {this.update({pollError});}
    finally {this.polling = false;}
  }

  async cancel():Promise<void> {
    if (this.closed || !this.state.owned || !this.busy || this.state.cancelPending || this.state.operation?.active === false) return;
    this.update({cancelRequested:true, error:null});
    const id = this.state.id;
    if (id === null) return;
    this.update({cancelPending:true});
    try {await this.client.cancel(id);}
    catch (error) {if (this.state.id === id) this.update({error});}
    finally {if (this.state.id === id) this.update({cancelPending:false});}
  }

  async guidance(kind:'link'|'copy', resourceId:string, index:number):Promise<void> {
    if (this.closed || this.state.guidancePending !== null) return;
    const key = `${kind}:${resourceId}:${index}`;
    this.update({guidancePending:key, error:null, copied:null});
    try {
      if (kind === 'link') await this.client.openLink(resourceId, index);
      else await this.client.copyCommand(resourceId, index);
      this.update({copied:kind === 'copy' ? key : null});
    } catch (error) {this.update({error});}
    finally {this.update({guidancePending:null});}
  }

  currentEnvironment(draft:SettingsDraft):OcrEnvironment|null {
    this.observeDraft(draft);
    return !this.closed && !this.busy && this.state.checkedRevision === this.revision
      ? this.state.view?.environment ?? null : null;
  }

  apply(draft:SettingsDraft):SettingsDraft|null {
    const environment = this.currentEnvironment(draft);
    if (environment === null) return null;
    const next = {...draft, environment:environmentDraft(environment)};
    this.update({view:this.state.view ? {...this.state.view, environment:null} : null, checkedRevision:null, applied:true});
    return next;
  }

  close():void {
    if (this.closed) return;
    const id = this.state.owned && this.state.operation?.active !== false ? this.state.id : null;
    this.closed = true;
    if (id !== null) void this.client.cancel(id).catch(() => {});
  }
}

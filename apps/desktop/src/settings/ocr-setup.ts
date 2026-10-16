import type {EnvironmentDraft, SettingsDraft} from '../state.ts';
import type {Fault, OcrEnvironment} from '../types.ts';

export interface LocalizedText {en:string; ja:string}
export interface SetupLink {label:LocalizedText; url:string}
export interface SetupItem {
  id:string; name:LocalizedText; state:string; detail:LocalizedText;
  downloadable:boolean; links:SetupLink[]; commands:string[];
}
export type NativeSelection = {method:'homebrew'} | {method:'folders'; paths:string[]};
export interface ResolvedResources {model_root:string|null; runtime_path:string|null; native_library_paths:string[]|null}
export type EvaluationOutcome = 'passed' | 'failed' | 'not_run' | 'incomplete';
export interface EvaluationEvidence {report:string; measured_source_revision:string; execution_sha256:string; evaluator_revision:string}
export interface EvaluationSummary {
  target:string; provider:string; criteria:string; evidence:EvaluationEvidence|null;
  rules:{rule_id:string; outcome:EvaluationOutcome}[];
}
// SDK catalog projection. Presentation and recorded results are display-only, never persisted or used to admit a choice.
export interface OcrProfileChoice {
  profile:string; model:string; language:string; provider:string; runtime_profile:string; preprocessing:string;
  display_name:string; variant:string|null; resource_id:string; evaluations:EvaluationSummary[];
}
// `profile`/`resource_id` bind the reply to the selection it was produced for.
export interface SetupView {
  items:SetupItem[]; environment:OcrEnvironment|null; resolved:ResolvedResources; native_methods:NativeSelection['method'][];
  profiles:OcrProfileChoice[]; default_profile:string; target:string; profile:string; resource_id:string;
}
export interface SetupProgress {stage:string; resource_id:string; bytes:number; total:number}
export interface SetupOperation {
  id:string; active:boolean; progress:SetupProgress; error:Fault|null; cleanup_error:Fault|null; result:SetupView|null;
}
export interface SetupClient {
  catalog:() => Promise<SetupView>;
  start:(resourceId:string, profile:string, environment:OcrEnvironment|null, nativeSelection:NativeSelection|null) => Promise<string>;
  poll:() => Promise<SetupOperation|null>;
  cancel:(operationId:string) => Promise<void>;
  pickFolder:() => Promise<string|null>;
  openLink:(resourceId:string, index:number) => Promise<void>;
  copyCommand:(resourceId:string, index:number) => Promise<void>;
}
export interface SetupState {
  view:SetupView|null; catalogPending:boolean; starting:string|null; resourceId:string|null; profile:string|null; id:string|null; operation:SetupOperation|null;
  owned:boolean; cancelRequested:boolean; cancelPending:boolean; error:unknown; pollError:unknown; unavailable:boolean;
  guidancePending:string|null; copied:string|null; checkedRevision:number|null; adopted:boolean;
  nativeSelection:NativeSelection|null; pickerPending:boolean; pickerStale:boolean;
}
export const INITIAL_SETUP:SetupState = {
  view:null, catalogPending:false, starting:null, resourceId:null, profile:null, id:null, operation:null,
  owned:false, cancelRequested:false, cancelPending:false, error:null, pollError:null, unavailable:false,
  guidancePending:null, copied:null, checkedRevision:null, adopted:false,
  nativeSelection:null, pickerPending:false, pickerStale:false,
};

export function profileChoice(profiles:readonly OcrProfileChoice[], profile:string):OcrProfileChoice|null {
  return profiles.find(choice => choice.profile === profile) ?? null;
}

// A blank selection means the explicit SDK default, never the first listed choice.
export function effectiveChoice(view:SetupView|null, profile:string):OcrProfileChoice|null {
  return view === null ? null : profileChoice(view.profiles, profile || view.default_profile);
}

// Rules recorded for this host target and the choice's provider; null when none ran.
export function evaluationCount(choice:OcrProfileChoice, target:string):{passed:number; total:number}|null {
  const rules = choice.evaluations.filter(summary => summary.target === target && summary.provider === choice.provider).flatMap(summary => summary.rules);
  if (rules.every(rule => rule.outcome === 'not_run')) return null;
  return {passed:rules.filter(rule => rule.outcome === 'passed').length, total:rules.length};
}

export type EvaluationTag = 'fast' | 'speed_attention' | 'speed_unmeasured' | 'speed_incomplete'
  | 'reading_good' | 'reading_attention' | 'reading_unmeasured' | 'reading_incomplete'
  | 'runtime_attention' | 'runtime_incomplete' | 'not_evaluated' | 'unverified';

const READING_RULES = ['text_order', 'region_count', 'geometry', 'unexpected_detection', 'confidence', 'stability', 'vocabulary_coverage'];

// Interpret criteria v1 only: relative_latency passes when every workload's p50/p95 is <= 80% of the reference.
export function evaluationTags(choice:OcrProfileChoice, target:string):EvaluationTag[] {
  const summary = choice.evaluations.find(item => item.target === target && item.provider === choice.provider);
  if (!summary || summary.rules.every(rule => rule.outcome === 'not_run')) return ['not_evaluated'];
  if (summary.criteria !== 'tools/ocr-models/criteria-v1.json' || summary.evidence === null) return ['unverified'];
  const outcome = (id:string) => summary.rules.find(rule => rule.rule_id === id)?.outcome ?? 'not_run';
  const speed = outcome('relative_latency');
  const tags:EvaluationTag[] = [speed === 'passed' ? 'fast' : speed === 'failed' ? 'speed_attention'
    : speed === 'incomplete' ? 'speed_incomplete' : 'speed_unmeasured'];
  const reading = READING_RULES.map(outcome);
  tags.push(reading.includes('failed') ? 'reading_attention' : reading.every(result => result === 'passed') ? 'reading_good'
    : reading.every(result => result === 'not_run') ? 'reading_unmeasured' : 'reading_incomplete');
  const runtime = summary.rules.filter(rule => rule.rule_id !== 'relative_latency' && !READING_RULES.includes(rule.rule_id));
  if (runtime.some(rule => rule.outcome === 'failed')) tags.push('runtime_attention');
  else if (runtime.some(rule => rule.outcome === 'incomplete')) tags.push('runtime_incomplete');
  return tags;
}

// A model root holds one model set: switching to another set clears it so Recheck or Download selects matching files.
export function selectProfile(draft:EnvironmentDraft, profile:string, view:SetupView|null):EnvironmentDraft {
  const before = effectiveChoice(view, draft.profile)?.resource_id;
  const after = effectiveChoice(view, profile)?.resource_id;
  return before !== undefined && after !== undefined && before !== after ? {...draft, profile, model_root:''} : {...draft, profile};
}

// Recheck accepts partial hints; Save still requires a complete catalog tuple.
export function setupEnvironment(draft:EnvironmentDraft, choice:OcrProfileChoice|null):OcrEnvironment|null {
  const libraries = draft.library_paths.split(/\r?\n/).map(path => path.trim()).filter(Boolean);
  const modelRoot = draft.model_root.trim();
  const runtimePath = draft.runtime_path.trim();
  if (!draft.profile && !modelRoot && !runtimePath && !libraries.length) return null;
  return {
    profile:draft.profile, model:choice?.model ?? '', language:choice?.language ?? '', provider:choice?.provider ?? '',
    runtime_profile:choice?.runtime_profile ?? '', model_root:modelRoot, runtime_path:runtimePath, native_library_paths:libraries,
  };
}

// One dialog owns the session across category switches, until close and backend settlement.
export class OcrSetupSession {
  private closed = false;
  private state:SetupState = {...INITIAL_SETUP};
  private draft:SettingsDraft;
  private environmentKey:string;
  private revision = 0;
  private operationRevision:number|null = null;
  // Model set of the requested profile when the owned operation started.
  private modelResource:string|null = null;
  private polling = false;
  private pickerToken = 0;
  private client:SetupClient;
  private publish:(state:SetupState) => void;
  private onDraft:(draft:SettingsDraft) => void;

  constructor(client:SetupClient, draft:SettingsDraft, publish:(state:SetupState) => void, onDraft:(draft:SettingsDraft) => void) {
    this.client = client;
    this.publish = publish;
    this.onDraft = onDraft;
    this.draft = draft;
    this.environmentKey = JSON.stringify(draft.environment);
  }

  get snapshot():SetupState {return this.state;}
  get busy():boolean {return this.state.pickerPending || this.state.cancelPending || this.state.starting !== null || Boolean(this.state.operation?.cleanup_error) || (this.state.id !== null && this.state.operation?.active !== false);}
  get draftRevision():number {return this.revision;}

  observeDraft(draft:SettingsDraft):void {
    this.draft = draft;
    const key = JSON.stringify(draft.environment);
    if (key !== this.environmentKey) {this.environmentKey = key; this.revision += 1;}
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
      this.update({operation, owned:false, checkedRevision:null, adopted:false, id:operation?.id ?? null, resourceId:operation?.progress.resource_id ?? null, profile:null, pollError:null});
    } catch (pollError) {this.update({pollError});}
    finally {this.update({catalogPending:false});}
  }

  async start(resourceId:string, draft:SettingsDraft):Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending) return;
    const view = this.state.view;
    // The SDK default fills only a blank selection; an unknown explicit profile is sent unchanged for the backend to refuse.
    const profile = draft.environment.profile || view?.default_profile;
    if (!profile) return;
    this.observeDraft(draft);
    this.operationRevision = this.revision;
    this.modelResource = view === null ? null : profileChoice(view.profiles, profile)?.resource_id ?? null;
    this.update({starting:resourceId, resourceId, profile, owned:true, id:null, operation:null, checkedRevision:null, adopted:false,
      cancelRequested:false, cancelPending:false, error:null, pollError:null, unavailable:false});
    try {
      const inspect = resourceId === 'inspect';
      const hints = inspect ? setupEnvironment(draft.environment, view === null ? null : profileChoice(view.profiles, draft.environment.profile)) : null;
      const id = await this.client.start(resourceId, profile, hints, inspect ? this.state.nativeSelection : null);
      if (this.closed) {
        // A late Start belongs to this dialog, never to the host's current job.
        try {await this.client.cancel(id);} catch { /* Backend admission retains ownership. */ }
        return;
      }
      this.update({id, starting:null});
      if (this.state.cancelRequested) await this.cancel();
    } catch (error) {this.update({starting:null, owned:false, error});}
  }

  private adopt(view:SetupView):boolean {
    const requested = this.state.profile;
    // Shared assets may serve several profiles; a reply counts only for the exact profile and model set requested.
    if (requested === null || view.profile !== requested || view.resource_id !== this.modelResource) return false;
    const current = this.draft.environment;
    const next = {...current};
    const inspect = this.state.resourceId === 'inspect';
    const models = this.state.resourceId === view.resource_id;
    if (view.resolved.model_root && (models || (inspect && !current.model_root.trim()))) {
      next.model_root = view.resolved.model_root;
    }
    if (view.resolved.runtime_path && (this.state.resourceId === 'onnxruntime' || (inspect && !current.runtime_path.trim()))) {
      next.runtime_path = view.resolved.runtime_path;
    }
    if (inspect && view.resolved.native_library_paths?.length && (this.state.nativeSelection !== null || !current.library_paths.trim())) {
      next.library_paths = view.resolved.native_library_paths.join('\n');
    }
    const verified = inspect
      ? Boolean(view.resolved.model_root || view.resolved.runtime_path || view.resolved.native_library_paths?.length)
      : models ? Boolean(view.resolved.model_root)
        : this.state.resourceId === 'onnxruntime' && Boolean(view.resolved.runtime_path);
    if (verified && !next.profile) next.profile = requested;
    if (next.profile === current.profile && next.model_root === current.model_root && next.runtime_path === current.runtime_path && next.library_paths === current.library_paths) return false;
    const draft = {...this.draft, environment:next};
    this.observeDraft(draft);
    this.onDraft(draft);
    return true;
  }

  async poll():Promise<void> {
    const id = this.state.id;
    if (this.closed || id === null || this.state.operation?.active === false || !this.busy || this.polling) return;
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
      if (!operation.active && operation.progress.stage === 'complete' && !operation.error && !operation.cleanup_error && operation.result !== null) {
        const result = operation.result;
        const previous = this.state.view;
        next.view = this.state.resourceId === 'inspect' || previous === null ? result : {
          ...result,
          items:previous.items.map(item => item.id === this.state.resourceId
            ? result.items.find(candidate => candidate.id === item.id) ?? item : item),
          resolved:{...previous.resolved,
            ...(this.state.resourceId === this.modelResource ? {model_root:result.resolved.model_root} : {}),
            ...(this.state.resourceId === 'onnxruntime' ? {runtime_path:result.resolved.runtime_path} : {})},
        };
        const current = this.operationRevision === this.revision;
        next.adopted = !this.state.cancelRequested && current && this.adopt(operation.result);
        next.checkedRevision = current ? this.revision : this.operationRevision;
      }
      this.update(next);
    } catch (pollError) {this.update({pollError});}
    finally {this.polling = false;}
  }

  async useHomebrew(draft:SettingsDraft):Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending || !this.state.view?.native_methods.includes('homebrew')) return;
    this.revision += 1;
    this.update({nativeSelection:{method:'homebrew'}, pickerStale:false});
    await this.start('inspect', draft);
  }

  async useManual(draft:SettingsDraft):Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending) return;
    this.revision += 1;
    this.update({nativeSelection:null, pickerStale:false});
    await this.start('inspect', draft);
  }

  async pickFolder(draft:SettingsDraft, add = false):Promise<void> {
    if (this.closed || this.busy || this.state.catalogPending || !this.state.view?.native_methods.includes('folders')) return;
    const selected = this.state.nativeSelection;
    const paths = add && selected?.method === 'folders' ? selected.paths : [];
    if (paths.length >= 8) return;
    this.observeDraft(draft);
    const revision = this.revision;
    const token = ++this.pickerToken;
    this.update({pickerPending:true, pickerStale:false, error:null});
    try {
      const path = await this.client.pickFolder();
      if (this.closed || token !== this.pickerToken) return;
      if (path === null) {this.update({pickerPending:false}); return;}
      if (revision !== this.revision) {this.update({pickerPending:false, pickerStale:true}); return;}
      const folders = paths.includes(path) ? [...paths] : [...paths, path];
      this.revision += 1;
      this.update({pickerPending:false, nativeSelection:{method:'folders', paths:folders}});
      await this.start('inspect', this.draft);
    } catch (error) {if (token === this.pickerToken) this.update({pickerPending:false, error});}
  }

  async cancel():Promise<void> {
    if (this.closed) return;
    if (this.state.pickerPending) {
      this.pickerToken += 1;
      this.update({pickerPending:false});
      return;
    }
    if (!this.state.owned || !this.busy || this.state.cancelPending || this.state.operation?.active === false) return;
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

  close():void {
    if (this.closed) return;
    const id = this.state.owned && this.state.operation?.active !== false ? this.state.id : null;
    this.closed = true;
    this.pickerToken += 1;
    if (id !== null) void this.client.cancel(id).catch(() => {});
  }
}

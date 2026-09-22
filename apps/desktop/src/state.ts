import type {ControllerView, Fault, LogEntry, OcrEnvironment, Schema, Json} from './types.ts';

export const DISCLOSURE_LIMIT = 512 * 1024;

export function faultSummary(value:Fault|Record<string,Json>, includeMessage = false):string {
  return boundedText([
    text(value.category) ?? 'Error', text(record(value.context).stage),
    includeMessage ? text(value.message) : null,
  ].filter(Boolean).join(' · '), 1024).text;
}
export function acceptController(current:ControllerView, incoming:ControllerView, expectedRun:string|null):ControllerView {
  return expectedRun !== null && incoming.run !== expectedRun ? current : incoming;
}

export function record(value:Json|undefined):Record<string,Json> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

export function text(value:Json|undefined):string|null {
  return typeof value === 'string' && value !== '' ? value : null;
}

export interface LogStore {items:LogEntry[]; evicted:number}
export function retainLogs(store:LogStore, incoming:LogEntry[], limit:number):LogStore {
  if (!Number.isInteger(limit) || limit < 1 || limit > 10000) throw new Error('GUI log limit must be an integer in 1..=10000');
  const discard = Math.max(0, store.items.length + incoming.length - limit);
  const oldDiscard = Math.min(discard,store.items.length);
  const newDiscard = discard - oldDiscard;
  return {items:[...store.items.slice(oldDiscard),...incoming.slice(newDiscard)],evicted:store.evicted+discard};
}

// Only absent top-level options receive defaults; nested drafts stay explicit.
export function defaultDraft(schema:Schema):Record<string,Json> {
  return Object.fromEntries(Object.entries(schema.properties ?? {})
    .filter(([,node])=>node.default !== undefined)
    .map(([key,node])=>[key,structuredClone(node.default!)]));
}

function exactIntegerText(text: string, number: number): boolean {
  const match = /^-?(\d+)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(text.trim())!;
  const fraction = match[2] ?? '';
  const digits = (match[1] + fraction).replace(/^0+/, '');
  const significant = digits.replace(/0+$/, '');
  if (!significant) return number === 0;
  const integer = String(Math.abs(number));
  const integerSignificant = integer.replace(/0+$/, '');
  const exponent = Number(match[3] ?? 0) - fraction.length + digits.length - significant.length;
  return significant === integerSignificant && exponent === integer.length - integerSignificant.length;
}

// Numeric editor text is preserved until the command boundary, never coerced to null or zero.
export function readDraft(schema: Schema, draft: Record<string, Json>) {
  const errors: Record<string, string> = {};
  function convert(node: Schema, value: Json, path: string): Json {
    if ((node.type === 'number' || node.type === 'integer') && (typeof value === 'string' || typeof value === 'number')) {
      const number = typeof value === 'string' ? Number(value) : value;
      if (!Number.isFinite(number) || (typeof value === 'string' && !/^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/.test(value.trim()))) {
        errors[path] = 'Enter a finite number; an empty field is not zero.';
        return value;
      }
      if (Object.is(number, -0)) {
        errors[path] = 'Negative zero cannot round-trip through desktop JSON without losing its sign.';
        return value;
      }
      if (node.type === 'integer' && (!Number.isSafeInteger(number) || (typeof value === 'string' && !exactIntegerText(value, number)))) {
        errors[path] = 'Enter an exact integer within the JavaScript safe integer range.';
        return value;
      }
      if (typeof value === 'string' && /^-?(?:0|[1-9]\d*)$/.test(value.trim()) && BigInt(value.trim()) !== BigInt(number)) {
        errors[path] = 'This integer cannot be represented exactly by the numeric editor.';
        return value;
      }
      return number;
    }
    if (node.type === 'object' && value !== null && typeof value === 'object' && !Array.isArray(value)) {
      return Object.fromEntries(Object.entries(value).map(([key, child]) => [key,
        node.properties?.[key] ? convert(node.properties[key], child, `${path}.${key}`) : child]));
    }
    if (node.type === 'array' && Array.isArray(value) && node.items) {
      return value.map((child, index) => convert(node.items!, child, `${path}[${index}]`));
    }
    return value;
  }
  return {values: convert(schema, draft, '$') as Record<string, Json>, errors};
}

export function verifiedCleanup(result:ControllerView['result']):boolean {
  if (!result || result.forced !== false || result.exit_code !== 0) return false;
  const cleanup = result.cleanup;
  return cleanup !== null && typeof cleanup === 'object' && !Array.isArray(cleanup) && cleanup.clean === true;
}

// Preparation faults settle before any child: their context may carry the only cleanup truth.
export function cleanupLabel(result:ControllerView['result'], fallback:Record<string,Json>):string {
  if (verifiedCleanup(result)) return 'Clean';
  const cleanup = record(result?.cleanup ?? fallback.cleanup);
  if (!result && cleanup.clean === true && cleanup.child_started === false) return 'Clean · no child started';
  if (cleanup.clean === false || result?.forced === true || (typeof result?.exit_code === 'number' && result.exit_code !== 0)) return 'Incomplete / not clean';
  return 'Unverified';
}

// Supported OCR tuples mirror the pinned engine's accepted set; the backend Check/Start
// validation remains authoritative and refuses anything else without fallback.
export const ENVIRONMENT_LANGUAGE = 'horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1';
export const ENVIRONMENT_PROVIDER = 'cpu';
export const ENVIRONMENT_RUNTIME_PROFILE = 'onnxruntime-1.29.0-api17-cpu';
export const SUPPORTED_PROFILES: readonly {profile:string; model:string; label:string}[] = [
  {profile:'g-004-rapidocr-ppocrv4-det-v6-rec-small-v1', model:'g-004-rapidocr-ppocrv4-det-v6-rec-small-v1', label:'G-004 · RapidOCR PP-OCRv4 det v6 / rec small v1'},
  {profile:'phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2', model:'phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2', label:'Phase 3.1 · bounded detector v2'},
];

export interface EnvironmentDraft {profile:string; model_root:string; runtime_path:string; library_paths:string}

export function environmentDraft(saved:OcrEnvironment|null):EnvironmentDraft {
  return saved
    ? {profile:saved.profile, model_root:saved.model_root, runtime_path:saved.runtime_path, library_paths:saved.native_library_paths.join('\n')}
    : {profile:'', model_root:'', runtime_path:'', library_paths:''};
}

// A wholly blank draft means unconfigured; anything else must be a complete supported tuple.
export function readEnvironment(draft:EnvironmentDraft):{environment:OcrEnvironment|null; errors:Record<string,string>} {
  const modelRoot = draft.model_root.trim();
  const runtimePath = draft.runtime_path.trim();
  const libraries = draft.library_paths.split(/\r?\n/).map(line => line.trim()).filter(line => line !== '');
  if (!draft.profile && !modelRoot && !runtimePath && libraries.length === 0) return {environment:null, errors:{}};
  const errors:Record<string,string> = {};
  const supported = SUPPORTED_PROFILES.find(item => item.profile === draft.profile);
  if (!supported) errors.profile = draft.profile ? 'This saved profile is not supported by this desktop build.' : 'Choose a supported OCR profile.';
  if (!modelRoot) errors.model_root = 'Enter the model root directory.';
  if (!runtimePath) errors.runtime_path = 'Enter the OCR runtime library path.';
  if (Object.keys(errors).length > 0 || !supported) return {environment:null, errors};
  return {environment:{
    model:supported.model, profile:supported.profile, language:ENVIRONMENT_LANGUAGE, provider:ENVIRONMENT_PROVIDER,
    runtime_profile:ENVIRONMENT_RUNTIME_PROFILE, model_root:modelRoot, runtime_path:runtimePath, native_library_paths:libraries,
  }, errors};
}

export function sameEnvironment(left:OcrEnvironment|null, right:OcrEnvironment|null):boolean {
  if (left === null || right === null) return left === right;
  return left.model === right.model && left.profile === right.profile && left.language === right.language
    && left.provider === right.provider && left.runtime_profile === right.runtime_profile
    && left.model_root === right.model_root && left.runtime_path === right.runtime_path
    && left.native_library_paths.length === right.native_library_paths.length
    && left.native_library_paths.every((path, index) => path === right.native_library_paths[index]);
}

// What a Check observed when it was requested; the backend result carries the derived identities.
export interface CheckAssociation {
  operation:string; environment:OcrEnvironment|null; descriptorPath:string|null; packageInventoryIdentity:string|null;
}
export interface CheckContext {
  saved:OcrEnvironment|null; draftDirty:boolean; descriptorPath:string|null; packageInventoryIdentity:string|null;
}

// A check is evidence for exactly what it observed; any later edit detaches it.
export function staleReasons(association:CheckAssociation, current:CheckContext):string[] {
  const reasons:string[] = [];
  if (!sameEnvironment(association.environment, current.saved)) reasons.push('the saved environment changed');
  if (current.draftDirty) reasons.push('the environment draft has unsaved edits');
  if (association.descriptorPath !== current.descriptorPath) reasons.push('a different corpus descriptor is selected');
  if (association.packageInventoryIdentity !== current.packageInventoryIdentity) reasons.push('a different package is inspected');
  return reasons;
}

export function boundedText(source:string, limit:number):{text:string; truncated:number} {
  if (!Number.isInteger(limit) || limit < 1) throw new Error('disclosure limit must be a positive integer');
  return source.length <= limit ? {text:source, truncated:0} : {text:source.slice(0, limit), truncated:source.length - limit};
}

// Initialization truth comes from the child's own milestones, never from a status word.
export function initializationLabel(progress:Record<string,Json>[]):string {
  const events = progress.map(event => text(event.event));
  if (events.includes('BackendInitialized')) return 'Initialized · child reported BackendInitialized';
  if (events.includes('BackendInitializationStarted')) return 'Started · completion not reported';
  if (events.includes('EnginePreparationStarted')) return 'Not reached · preparation only';
  return 'Not attempted';
}

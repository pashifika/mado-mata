import type {Locale} from './i18n.ts';
import type {AuthoringRef, ControllerView, Fault, Json, NativeSelectionView, CaptureCacheReceipt} from './types.ts';

// Saved-image recognition authoring. The main window keeps this state inside the Edit session; the detached preview
// window renders a trimmed snapshot of it and relays on-image edits back (see PREVIEW_*). Geometry is stored in
// original frame pixels and content-relative normalized edges; display zoom never changes stored coordinates.

// Host data model: mirrors `mado_runtime_comparison::recognition` (serde snake_case, optional fields as null).
export const DOCUMENT_VERSION = 1;
export const ROUNDING_RULE = 1;
export const MAX_DEFINITIONS = 256;
export const MAX_EXPECTED_BYTES = 4096;
export const MAX_NAME_BYTES = 512;
export const MAX_DOCUMENT_BYTES = 262_144;
// Metadata Undo retains at most this many actions and bytes; it never holds frame pixels or trial results.
export const UNDO_ENTRIES = 64;
export const UNDO_BYTES = 1_048_576;
// Upstream template match defaults (pinned manifest `min_score`/`max_results`); authoring does not tune them.
export const TEMPLATE_DEFAULTS = {threshold: 0.9, max_results: 8} as const;
export const ZOOM_LEVELS = [25, 50, 75, 100, 150, 200, 300, 400] as const;

export interface PixelRect {x:number; y:number; width:number; height:number}
export interface GeometryBasis {frame_width:number; frame_height:number; content:PixelRect}
// Content-relative half-open edges: 0 <= u0 < u1 <= 1 and 0 <= v0 < v1 <= 1.
export interface NormalizedRect {u0:number; v0:number; u1:number; v1:number}
export interface SavedCrop {asset:string; sha256:string; width:number; height:number}
export interface TemplateSettings {search_region:NormalizedRect; threshold:number; max_results:number}
export interface TemplateRights {license:string; created_by:string; created_for:string|null; reviewed:boolean}
export type RecognitionKind = 'ocr' | 'template';
// `region` is the OCR ROI or the template pattern crop; `template.search_region` is the separate search ROI.
// `expected` is optional Script reference text: grouped OCR Copy carries it only as an escaped comment beside its
// zone. It is never sent to the engine, matched, awaited or used to judge a trial. `saved` is host-derived.
export interface RecognitionDefinition {
  id:string; name:string; revision:number; kind:RecognitionKind; region:NormalizedRect;
  expected:string|null; template:TemplateSettings|null; saved:SavedCrop|null;
}
export interface RecognitionDocument {
  version:number; rounding:number; basis:GeometryBasis; definitions:RecognitionDefinition[]; template_rights:TemplateRights|null;
}
export interface CaptureDocument {capture_id:string; document:RecognitionDocument}
// `game_content` is the reusable `recognitionBasis` setup, `ocr_recognize` one grouped request for every checked OCR
// Trial row, `template_recognize` the selected saved template.
export type SnippetKind = 'game_content' | 'ocr_recognize' | 'template_recognize';

// Owner-scoped frame descriptor; pixels stay in the host. Compatible frames reuse confirmed content setup.
// `historical` marks frozen earlier pixels (native or cached reload) independently of any acquisition time.
export interface RecognitionFrame {id:string; width:number; height:number; revision:number; confirmed:boolean; historical:boolean; historical_capture_at_ms:number|null}
// Read-only host image policy.
export interface ImagePolicy {
  input_bytes:number; input_pixels:number; crop_bytes:number; crop_pixels:number; package_image_bytes:number;
  package_non_image_bytes:number; package_bytes:number; package_decoded_bytes:number; replay_decoded_bytes:number;
  payload_bytes:number; preview_pixels:number;
}
// `max_ocr_zones` is the pinned public grouped-request bound reported by the fixed engine child; null until that
// child answered (there is no fallback value).
export interface RecognitionCapabilities {
  max_ocr_zones:number|null; diagnostic_regions:number; diagnostic_bytes:number; expected_bytes:number; image_policy:ImagePolicy;
}
// One host read. `revision` is the saved package revision and `saved_document` the document in it; `document` is
// the host-validated draft at `document_revision`, which confirmation, trials, saves and Copy name.
export interface RecognitionView {
  owner:AuthoringRef; revision:string; document:RecognitionDocument|null; saved_document:RecognitionDocument|null;
  capture_id:string|null; captures:CaptureDocument[]; saved_captures:CaptureDocument[]; migration_required:boolean;
  document_revision:number; basis_confirmed:boolean; other_bases_confirmed:boolean; frame:RecognitionFrame|null; capabilities:RecognitionCapabilities;
  staged_crop_ids:string[]; stale_crop_ids:string[]; staged_crop_sources:Record<string,string>;
  // Identity of the effective App OCR configuration; a change makes earlier trials stale.
  configuration_revision:string; trial:RecognitionTrial|null;
}

export interface OcrRegion {text:string; confidence:number|null; bounds:PixelRect; geometry:[number, number][]}
export interface OcrZoneResult {id:string; outcome:'recognized' | 'no_match'; regions:OcrRegion[]}
export interface TemplateMatch {score:number; bounds:PixelRect}
export type TrialDiagnostics =
  | {kind:'ocr'; text_contract:string; zones:OcrZoneResult[]}
  | {kind:'template'; id:string; threshold:number; outcome:'matched' | 'no_match'; matches:TemplateMatch[]};
// The runner's terminal result inside `controller.result`. Primary and cleanup outcomes stay separate.
export interface TrialEnvelope {
  version:number; operation:'recognition_trial'; environment_identity:string|null; primary:Fault|null; cleanup:Json;
  child_reaped:boolean; forced:boolean; exit_code:number|null; result:TrialDiagnostics|null;
}
// A settled trial with the inputs the host captured for it; `stale` is the host's own input-change verdict.
export interface RecognitionTrial {
  owner:AuthoringRef; revision:string; document_revision:number; frame_id:string|null; frame_revision:number;
  capture_id:string;
  configuration_revision:string; sample_id:string|null; stale:boolean; controller:ControllerView;
}
export interface CopyResult {capture_id:string; source:string; basis:GeometryBasis; verified:boolean; document_revision:number; definition_ids:string[]}

export function trialEnvelope(trial:RecognitionTrial):TrialEnvelope|null {
  const result = trial.controller.result;
  return result !== null && result.operation === 'recognition_trial' ? result as unknown as TrialEnvelope : null;
}

// Local freshness stamps. `marks` change whenever a definition's recognition inputs change (never reused, so Undo
// cannot revive an old result); `basis` changes with the frame or content rectangle.
export interface TrialStamp {basis:number; marks:Record<string, number>}
export interface TrialTicket {
  token:string; revision:string; kind:'frame' | 'sample'; frame_id:string|null; document_revision:number;
  capture_id:string;
  selected_ids:string[]; sample_id:string|null; stamp:TrialStamp;
}
// `fault` is a refusal before any trial settled. A ticketless record is a trial the host retained from before
// this state existed; it is never shown as current.
export interface TrialRecord {ticket:TrialTicket|null; trial:RecognitionTrial|null; fault:Fault|null}
// A Copy captures every input and its selection generation; changing a checked set and restoring it does not
// revive copied source. Recognition-input (or, for grouped OCR, source-input) marks likewise prevent geometry Undo
// from reviving a Copy. Setup source is the basis value itself, so it is compared by value.
export interface CopyStamp {basis:number; definitions:Record<string, {revision:number; mark:number}>; source:string|null; selection:number}
export interface CopyTicket {capture_id:string; token:string; revision:string; document_revision:number; definition_ids:string[]; kind:SnippetKind; stamp:CopyStamp}
// `error` is the host refusal or the clipboard failure; either way no package file changed.
export interface CopyRecord {capture_id:string; definition_ids:string[]; stamp:CopyStamp; basis:GeometryBasis|null; verified:boolean; error:Fault|null}
export interface SyncTicket {capture_id:string; token:string; revision:string; document_revision:number; local_revision:number; frame_id:string|null; document:RecognitionDocument}
export interface ConfirmTicket {capture_id:string; token:string; revision:string; frame_id:string; document_revision:number}
export interface SaveTicket {
  token:string; revision:string; document_revision:number; crop_ids:string[]; crop_sources:Record<string,string>; document:RecognitionDocument;
  capture_id:string;
  // Selection stamp and recognition-input mark of each submitted crop, so a crop re-selected or changed while the
  // save was pending stays selected.
  crops:Record<string, [number, number]>;
}
export interface UndoEntry {
  capture_id:string; document:RecognitionDocument; selected:string|null; bytes:number;
  // Only selections removed with a definition; pixels remain in the host.
  crops:Record<string, {mark:number; source:string}>;
}
export type PreviewTool = 'zones' | 'content';
export type Zoom = 'fit' | number;
export interface PreviewDisplay {zoom:Zoom; tool:PreviewTool}

export type RecognitionNotice = 'definitionLimit' | 'documentLimit' | 'expectedLimit' | 'nameLimit' | 'invalidGeometry' | 'staleEdit' | 'confirmBeforeReplace';
export type RecognitionBlock =
  | 'noDocument' | 'noFrame' | 'unconfirmed' | 'confirmed' | 'running' | 'noCapability' | 'empty' | 'overLimit' | 'mixedKinds'
  | 'templateSingle' | 'noSample' | 'invalid' | 'noChanges' | 'otherCapture' | 'rights' | 'cropFrame' | 'kind' | 'templateUnsaved';
export type DefinitionIssue = 'name' | 'region' | 'search' | 'searchSmall';
export type Freshness = 'fresh' | 'stale' | 'historical';
export type CopyFreshness = 'current' | 'obsolete' | 'failed';

export interface RecognitionState {
  owner:AuthoringRef; view:RecognitionView;
  // The local draft; null until a document exists (a package without metadata and no loaded frame).
  document:RecognitionDocument|null;
  // Bumped by every local metadata change; the preview Undo fence.
  localRevision:number;
  clock:number; basis:number; marks:Record<string, number>;
  // Like `marks` (same never-reused clock) but blind to the saved crop, which grouped OCR source never carries.
  sourceMarks:Record<string, number>;
  // Shared by the list and the preview overlay.
  selected:string|null;
  // OCR definitions checked for the next grouped trial and grouped OCR Copy, in document order.
  trialIds:string[]; trialSelection:number;
  // Definitions whose next Save uses the original frame selected for each crop.
  cropIds:string[]; cropMarks:Record<string, number>; cropSources:Record<string,string>;
  // Only inactive captures; the active capture uses cropIds/cropMarks/cropSources.
  pendingCrops:Record<string,{ids:string[]; marks:Record<string,number>; sources:Record<string,string>}>;
  undo:UndoEntry[]; undoBytes:number; group:string|null;
  trial:TrialRecord|null; running:TrialTicket|null; copies:Partial<Record<SnippetKind, CopyRecord>>;
  // Preview zoom and tool, kept here so a reopened preview window resumes them.
  display:PreviewDisplay;
  nextId:number; notice:RecognitionNotice|null; error:Fault|null;
  nextIds:Record<string, number>;
}

const encoder = new TextEncoder();
export function utf8Bytes(text:string):number {
  return encoder.encode(text).length;
}

export function documentBytes(document:RecognitionDocument):number {
  return utf8Bytes(JSON.stringify(document));
}

export function captureDocuments(state:RecognitionState, document:RecognitionDocument|null = state.document):CaptureDocument[] {
  return state.view.captures.map(capture => capture.capture_id === state.view.capture_id && document
    ? {...capture, document} : capture);
}

export function aggregateDefinitions(state:RecognitionState):number {
  return captureDocuments(state).reduce((count, capture) => count + capture.document.definitions.length, 0);
}

function packageBytes(state:RecognitionState, document:RecognitionDocument):number {
  return utf8Bytes(JSON.stringify({version: 2, captures: captureDocuments(state, document)}));
}

export function canUndoRecognition(state:RecognitionState):boolean {
  return state.undo.some(entry => entry.capture_id === state.view.capture_id);
}

function clearCropUndo(state:RecognitionState, captureId:string|null = null):Pick<RecognitionState, 'undo' | 'undoBytes'> {
  const undo = state.undo.map(entry => {
    if ((captureId !== null && entry.capture_id !== captureId) || Object.keys(entry.crops).length === 0) return entry;
    return {...entry, crops: {}, bytes: entry.bytes - utf8Bytes(JSON.stringify(entry.crops)) + 2};
  });
  return {undo, undoBytes: undo.reduce((bytes, entry) => bytes + entry.bytes, 0)};
}

export function discardPixelCrops(state:RecognitionState):RecognitionState {
  const pendingCrops = Object.fromEntries(Object.entries(state.pendingCrops).filter(([id]) => id !== state.view.capture_id));
  return {...state, ...clearCropUndo(state, state.view.capture_id), pendingCrops,
    cropIds: [], cropMarks: {}, cropSources: {}, group: null};
}

// Structural equality for JSON-shaped host data, independent of key order.
export function sameJson(a:unknown, b:unknown):boolean {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null || Array.isArray(a) !== Array.isArray(b)) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every(key => Object.hasOwn(right, key) && sameJson(left[key], right[key]));
}

// ---------------------------------------------------------------------------------------------------------------
// Geometry. Frame pixel edges are half-open: a rectangle covers [left, right) x [top, bottom).

export interface Edges {left:number; top:number; right:number; bottom:number}
export interface Point {x:number; y:number}
export type Handle = 'move' | 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

export function rectEdges(rect:PixelRect):Edges {
  return {left: rect.x, top: rect.y, right: rect.x + rect.width, bottom: rect.y + rect.height};
}

export function edgesRect(edges:Edges):PixelRect {
  return {x: edges.left, y: edges.top, width: edges.right - edges.left, height: edges.bottom - edges.top};
}

function wholeNumber(value:number, min:number):boolean {
  return Number.isSafeInteger(value) && value >= min;
}

// Content inside the frame with at least one pixel, all integers.
export function validBasis(basis:GeometryBasis):boolean {
  const {frame_width: width, frame_height: height, content} = basis;
  return wholeNumber(width, 1) && wholeNumber(height, 1) && wholeNumber(content.x, 0) && wholeNumber(content.y, 0)
    && wholeNumber(content.width, 1) && wholeNumber(content.height, 1)
    && content.x + content.width <= width && content.y + content.height <= height;
}

// Rounding rule 1: low edges floor, high edges ceil, offset by the content origin. Invalid input maps to null
// (the host refuses it too) rather than being clipped.
export function mapRegion(region:NormalizedRect, basis:GeometryBasis):Edges|null {
  const {u0, v0, u1, v1} = region;
  if (!validBasis(basis) || ![u0, v0, u1, v1].every(value => Number.isFinite(value) && value >= 0 && value <= 1) || !(u0 < u1) || !(v0 < v1)) return null;
  const {x, y, width, height} = basis.content;
  const edges = {left: x + Math.floor(u0 * width), top: y + Math.floor(v0 * height), right: x + Math.ceil(u1 * width), bottom: y + Math.ceil(v1 * height)};
  return edges.right > edges.left && edges.bottom > edges.top ? edges : null;
}

const bits = new DataView(new ArrayBuffer(8));
// Adjacent double of a finite value in [0, 1].
function adjacent(value:number, direction:1 | -1):number {
  if (value === 0) return direction > 0 ? Number.MIN_VALUE : 0;
  bits.setFloat64(0, value);
  bits.setBigUint64(0, bits.getBigUint64(0) + BigInt(direction));
  return bits.getFloat64(0);
}

// The normalized low edge whose floor mapping lands exactly on integer offset `k` of `n` content pixels. `k / n`
// alone can land one pixel short after f64 rounding (1 / 49 * 49 < 1), so the value is nudged by ulps.
export function lowEdge(k:number, n:number):number {
  if (k <= 0) return 0;
  let value = k / n;
  for (let step = 0; step < 8 && Math.floor(value * n) < k; step++) value = adjacent(value, 1);
  for (let step = 0; step < 8 && Math.floor(value * n) > k; step++) value = adjacent(value, -1);
  return value;
}

// The normalized high edge whose ceil mapping lands exactly on integer offset `k` of `n` content pixels.
export function highEdge(k:number, n:number):number {
  if (k >= n) return 1;
  let value = k / n;
  for (let step = 0; step < 8 && Math.ceil(value * n) > k; step++) value = adjacent(value, -1);
  for (let step = 0; step < 8 && Math.ceil(value * n) < k; step++) value = adjacent(value, 1);
  return value;
}

// Normalized edges for integer frame-pixel edges inside the content rectangle; null when outside or empty.
export function regionFromEdges(edges:Edges, basis:GeometryBasis):NormalizedRect|null {
  if (!validBasis(basis) || ![edges.left, edges.top, edges.right, edges.bottom].every(Number.isSafeInteger)) return null;
  const {x, y, width, height} = basis.content;
  const left = edges.left - x, top = edges.top - y, right = edges.right - x, bottom = edges.bottom - y;
  if (left < 0 || top < 0 || right > width || bottom > height || right <= left || bottom <= top) return null;
  return {u0: lowEdge(left, width), v0: lowEdge(top, height), u1: highEdge(right, width), v1: highEdge(bottom, height)};
}

export const FULL_REGION: NormalizedRect = {u0: 0, v0: 0, u1: 1, v1: 1};

function clamp(value:number, min:number, max:number):number {
  return Math.min(Math.max(value, min), max);
}

// Continuous frame coordinates under a pointer. `rect` is the element's rendered client rectangle, which already
// reflects zoom, scroll and CSS transforms in CSS pixels; device-pixel ratio is not applied a second time.
export function clientToFrame(clientX:number, clientY:number, rect:{left:number; top:number; width:number; height:number}, width:number, height:number):Point {
  return {x: (clientX - rect.left) * width / rect.width, y: (clientY - rect.top) * height / rect.height};
}

// CSS pixels per frame pixel. Fit shows the whole frame in the viewport.
export function displayScale(zoom:Zoom, width:number, height:number, viewport:{width:number; height:number}):number {
  if (zoom !== 'fit') return zoom / 100;
  const scale = Math.min(viewport.width / width, viewport.height / height);
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

// The next listed percentage above or below the current effective scale.
export function stepZoom(scale:number, direction:1 | -1):number {
  const percent = scale * 100;
  const levels: readonly number[] = ZOOM_LEVELS;
  const next = direction > 0 ? levels.find(level => level > percent + 0.5) : [...levels].reverse().find(level => level < percent - 0.5);
  return next ?? (direction > 0 ? levels[levels.length - 1] : levels[0]);
}

// A new rectangle spanned by two pointer positions, rounded to pixel edges inside `bounds`; null when empty.
export function spanEdges(a:Point, b:Point, bounds:Edges):Edges|null {
  const x0 = clamp(Math.round(Math.min(a.x, b.x)), bounds.left, bounds.right);
  const x1 = clamp(Math.round(Math.max(a.x, b.x)), bounds.left, bounds.right);
  const y0 = clamp(Math.round(Math.min(a.y, b.y)), bounds.top, bounds.bottom);
  const y1 = clamp(Math.round(Math.max(a.y, b.y)), bounds.top, bounds.bottom);
  return x1 > x0 && y1 > y0 ? {left: x0, top: y0, right: x1, bottom: y1} : null;
}

// Moves or resizes `start` by whole frame pixels. Moves keep the size inside `bounds`; resized edges stay inside
// `bounds` and never cross, so the result keeps at least one pixel.
export function dragEdges(start:Edges, handle:Handle, dx:number, dy:number, bounds:Edges):Edges {
  const {left, top, right, bottom} = start;
  if (handle === 'move') {
    const x = clamp(left + dx, bounds.left, bounds.right - (right - left));
    const y = clamp(top + dy, bounds.top, bounds.bottom - (bottom - top));
    return {left: x, top: y, right: x + right - left, bottom: y + bottom - top};
  }
  const next = {...start};
  if (handle.includes('w')) next.left = clamp(left + dx, bounds.left, right - 1);
  if (handle.includes('e')) next.right = clamp(right + dx, left + 1, bounds.right);
  if (handle.includes('n')) next.top = clamp(top + dy, bounds.top, bottom - 1);
  if (handle.includes('s')) next.bottom = clamp(bottom + dy, top + 1, bounds.bottom);
  return next;
}

// The handle under `point`; `tolerance` is in frame pixels (the caller converts its CSS hit size by the scale).
// Inner tolerance shrinks for small rectangles so their body stays reachable.
export function hitHandle(edges:Edges, point:Point, tolerance:number):Handle|null {
  const {left, top, right, bottom} = edges;
  const {x, y} = point;
  if (x < left - tolerance || x > right + tolerance || y < top - tolerance || y > bottom + tolerance) return null;
  const tx = Math.min(tolerance, (right - left) / 3);
  const ty = Math.min(tolerance, (bottom - top) / 3);
  const vertical = y <= top + ty ? 'n' : y >= bottom - ty ? 's' : '';
  const horizontal = x <= left + tx ? 'w' : x >= right - tx ? 'e' : '';
  const handle = vertical + horizontal;
  if (handle) return handle as Handle;
  return x > left && x < right && y > top && y < bottom ? 'move' : null;
}

// The region part a press grabs: the selected definition first (its pattern, then only the edges and corners of its
// template search area, whose body never covers other regions or empty content to create in), then the others from
// the topmost. `edges` gives the part's shown frame edges.
export function hitRegion(definitions:readonly PreviewDefinition[], selected:string|null, point:Point, tolerance:number,
  edges:(definition:PreviewDefinition, part:'region' | 'search') => Edges|null):{definition:PreviewDefinition; part:'region' | 'search'; handle:Handle; edges:Edges}|null {
  const chosen = definitions.find(item => item.id === selected);
  const ordered = chosen ? [chosen, ...definitions.filter(item => item !== chosen).reverse()] : [...definitions].reverse();
  for (const definition of ordered) {
    const parts: ('region' | 'search')[] = definition === chosen && definition.search ? ['region', 'search'] : ['region'];
    for (const part of parts) {
      const shown = edges(definition, part);
      const handle = shown && hitHandle(shown, point, tolerance);
      if (shown && handle && (part === 'region' || handle !== 'move')) return {definition, part, handle, edges: shown};
    }
  }
  return null;
}

// ---------------------------------------------------------------------------------------------------------------
// Validation mirrored from the host so the UI can explain refusals before a round trip; the host stays authoritative.

export function definitionIssue(definition:RecognitionDefinition, basis:GeometryBasis):DefinitionIssue|null {
  if (!definition.name.trim() || definition.name.includes('\0') || utf8Bytes(definition.name) > MAX_NAME_BYTES) return 'name';
  const region = mapRegion(definition.region, basis);
  if (!region) return 'region';
  if (definition.kind === 'template') {
    const search = definition.template && mapRegion(definition.template.search_region, basis);
    if (!search) return 'search';
    const width = definition.saved?.width ?? region.right - region.left;
    const height = definition.saved?.height ?? region.bottom - region.top;
    if (search.right - search.left < width || search.bottom - search.top < height) return 'searchSmall';
  }
  return null;
}

function documentValid(document:RecognitionDocument):boolean {
  return validBasis(document.basis) && document.definitions.every(definition => definitionIssue(definition, document.basis) === null);
}

export function rightsValid(rights:TemplateRights|null):boolean {
  return rights !== null && rights.license.trim() !== '' && rights.created_by.trim() !== '' && rights.reviewed;
}

// ---------------------------------------------------------------------------------------------------------------
// State construction and host replies.

function fullBasis(frame:RecognitionFrame):GeometryBasis {
  return {frame_width: frame.width, frame_height: frame.height, content: {x: 0, y: 0, width: frame.width, height: frame.height}};
}

// The document the host holds; a frame without one shows an empty full-image document.
function hostDocument(view:RecognitionView):RecognitionDocument|null {
  if (view.document !== null || view.frame === null) return view.document;
  return {version: DOCUMENT_VERSION, rounding: ROUNDING_RULE, basis: fullBasis(view.frame), definitions: [], template_rights: null};
}

function maxRevision(...documents:(RecognitionDocument|null)[]):number {
  let max = 0;
  for (const document of documents) for (const definition of document?.definitions ?? []) max = Math.max(max, definition.revision);
  return max;
}

function idNumber(...documents:(RecognitionDocument|null)[]):number {
  let max = 0;
  for (const document of documents) for (const {id} of document?.definitions ?? []) {
    const match = /^r(\d{1,9})$/.exec(id);
    if (match) max = Math.max(max, Number(match[1]));
  }
  return max + 1;
}

// What a trial of the definition depends on; its name and Script reference text are not recognition inputs.
function recognitionInputs(definition:RecognitionDefinition):unknown {
  return [definition.kind, definition.region, definition.template, definition.saved?.sha256 ?? null];
}

// Stored content equality. A definition `revision` is fence bookkeeping (a newer one may name restored content), so
// it is ignored; basis, rights, definition order and every other field compare exactly, as the host compares saved
// correspondence.
function sameContent(a:RecognitionDocument|null, b:RecognitionDocument|null):boolean {
  if (a === null || b === null) return a === b;
  const {definitions: left, ...rest} = a;
  const {definitions: right, ...other} = b;
  return left.length === right.length && sameJson(rest, other)
    && left.every((definition, index) => sameJson({...definition, revision: 0}, {...right[index], revision: 0}));
}

function sameBasis(a:GeometryBasis|null|undefined, b:GeometryBasis|null|undefined):boolean {
  return a !== null && a !== undefined && b !== null && b !== undefined && sameJson(a, b);
}

// Recomputes freshness stamps after the document changed from `before`; pending crops retain their original source.
function stamp(state:RecognitionState, before:RecognitionDocument|null):RecognitionState {
  const after = state.document;
  let {clock, basis} = state;
  if (!sameBasis(before?.basis, after?.basis)) basis += 1;
  const previous = new Map(before?.definitions.map(definition => [definition.id, definition]) ?? []);
  const marks: Record<string, number> = {};
  const sourceMarks: Record<string, number> = {};
  for (const definition of after?.definitions ?? []) {
    const old = previous.get(definition.id);
    const mark = state.marks[definition.id];
    const sourceMark = state.sourceMarks[definition.id];
    marks[definition.id] = old && mark !== undefined && sameJson(recognitionInputs(old), recognitionInputs(definition)) ? mark : ++clock;
    // Grouped OCR source depends on the same inputs except the saved diagnostic crop (name and reference text are
    // covered by the definition revision).
    sourceMarks[definition.id] = old && sourceMark !== undefined
      && sameJson([old.kind, old.region, old.template], [definition.kind, definition.region, definition.template]) ? sourceMark : ++clock;
  }
  const order = after?.definitions.map(definition => definition.id) ?? [];
  const ocr = new Set(after?.definitions.filter(definition => definition.kind === 'ocr').map(definition => definition.id) ?? []);
  const cropIds = order.filter(id => state.cropIds.includes(id));
  const trialIds = order.filter(id => ocr.has(id) && state.trialIds.includes(id));
  return {...state, clock, basis, marks, sourceMarks,
    selected: state.selected !== null && order.includes(state.selected) ? state.selected : null,
    trialIds, trialSelection: state.trialSelection + (sameJson(trialIds, state.trialIds) ? 0 : 1),
    cropIds, cropMarks: Object.fromEntries(cropIds.map(id => [id, state.cropMarks[id]])),
    cropSources: Object.fromEntries(cropIds.filter(id => state.cropSources[id]).map(id => [id, state.cropSources[id]]))};
}

// A new frame invalidates trials and frame-bound copies, but not the metadata draft or the original pixels
// already selected for a pending crop. Different dimensions need an explicit author rebase before confirmation.
function adoptFrame(state:RecognitionState, previous:RecognitionFrame|null, frame:RecognitionFrame|null):RecognitionState {
  if ((previous?.id ?? null) === (frame?.id ?? null)) return state;
  const document = state.document ?? (frame === null ? null
    : {version: DOCUMENT_VERSION, rounding: ROUNDING_RULE, basis: fullBasis(frame), definitions: [], template_rights: null});
  const next = stamp({...state, document, localRevision: state.localRevision + 1}, state.document);
  return {...next, basis: next.basis === state.basis ? state.basis + 1 : next.basis,
    trialSelection: next.trialSelection + 1, display: {...next.display, tool: 'zones'}};
}

// Host-owned saved-crop references follow the host even under unsaved local edits.
function withSavedRefs(document:RecognitionDocument, host:RecognitionDocument|null):RecognitionDocument {
  if (host === null) return document;
  const saved = new Map(host.definitions.map(definition => [definition.id, definition.saved]));
  return {...document, definitions: document.definitions.map(definition => {
    const ref = saved.get(definition.id);
    return ref === undefined || sameJson(ref, definition.saved) ? definition : {...definition, saved: ref};
  })};
}

export function openRecognition(view:RecognitionView):RecognitionState {
  const empty: RecognitionState = {
    owner: view.owner, view: {...view, frame: null}, document: null, localRevision: 0,
    clock: maxRevision(view.document, view.saved_document), basis: 0, marks: {}, sourceMarks: {},
    selected: null, trialIds: [], trialSelection: 0, cropIds: [], cropMarks: {}, cropSources: {}, pendingCrops: {}, undo: [], undoBytes: 0, group: null,
    trial: view.trial ? {ticket: null, trial: view.trial, fault: null} : null, running: null, copies: {},
    display: {zoom: 'fit', tool: 'zones'}, nextId: idNumber(view.document, view.saved_document), notice: null, error: null,
    nextIds: {},
  };
  const state = adoptFrame(stamp({...empty, document: view.document}, null), null, view.frame);
  return {...state, view, selected: state.document?.definitions[0]?.id ?? null};
}

// A removed capture cannot retain pending pixel choices or an Undo path into its abandoned draft.
function reconcileCaptures(state:RecognitionState):RecognitionState {
  const ids = new Set(state.view.captures.map(capture => capture.capture_id));
  const undo = state.undo.filter(entry => ids.has(entry.capture_id));
  return {...state, undo, undoBytes: undo.reduce((bytes, entry) => bytes + entry.bytes, 0),
    pendingCrops: Object.fromEntries(Object.entries(state.pendingCrops).filter(([id, crops]) =>
      ids.has(id) && id !== state.view.capture_id && crops.ids.length > 0)),
    nextIds: Object.fromEntries(Object.entries(state.nextIds).filter(([id]) => ids.has(id)))};
}

// Applies a host read for the same owner. The local draft adopts the host document only when the host knew
// exactly this draft (`sent`); otherwise newer local edits are kept and only host-owned fields merge.
export function applyView(state:RecognitionState, view:RecognitionView, sent:RecognitionDocument|null = state.view.document):RecognitionState {
  if (view.owner.token !== state.owner.token || view.document_revision < state.view.document_revision) return state;
  if (view.capture_id !== state.view.capture_id) {
    if (view.document_revision <= state.view.document_revision) return state;
    const previousId = state.view.capture_id;
    const pendingCrops = previousId === null ? state.pendingCrops : {...state.pendingCrops,
      [previousId]: {ids: state.cropIds, marks: state.cropMarks, sources: state.cropSources}};
    const restored = view.capture_id === null ? null : pendingCrops[view.capture_id];
    const fresh = openRecognition(view);
    const nextIds = Object.fromEntries(view.captures.map(capture => [capture.capture_id,
      capture.capture_id === previousId ? state.nextId : state.nextIds[capture.capture_id] ?? 1]));
    const next = stamp({...fresh, clock: Math.max(state.clock, fresh.clock), marks: {}, sourceMarks: {},
      basis: state.basis + 1, localRevision: state.localRevision + 1, undo: state.undo, undoBytes: state.undoBytes,
      pendingCrops, cropIds: restored?.ids ?? [], cropMarks: restored?.marks ?? {}, cropSources: restored?.sources ?? {},
      nextIds, nextId: Math.max(fresh.nextId, view.capture_id ? nextIds[view.capture_id] ?? 1 : 1)}, null);
    return reconcileCaptures(next);
  }
  const local = state.document;
  const document = local === null || sameJson(local, sent) ? hostDocument(view) : withSavedRefs(local, view.document);
  const changed = !sameJson(document, local);
  let next = stamp({...state, view, document, localRevision: state.localRevision + (changed ? 1 : 0)}, local);
  next = adoptFrame(next, state.view.frame, view.frame);
  // The host re-reports its retained trial; keep the local ticket for the same run. Staleness never reverts.
  let trial = next.trial;
  if (view.trial !== null) {
    const known = trial?.trial;
    if (known && known.controller.run === view.trial.controller.run) trial = {...trial!, trial: {...view.trial, stale: view.trial.stale || known.stale}};
    else if (trial === null) trial = {ticket: null, trial: view.trial, fault: null};
  }
  return reconcileCaptures({...next, trial,
    clock: Math.max(next.clock, maxRevision(view.document, view.saved_document)),
    nextId: Math.max(next.nextId, idNumber(view.document, view.saved_document)),
    selected: next.selected ?? next.document?.definitions[0]?.id ?? null});
}

// The saved App OCR configuration changed: the retained trial is stale at once, before the next host read
// reports the new configuration identity.
export function invalidateConfiguration(state:RecognitionState):RecognitionState {
  const record = state.trial;
  return record?.trial && !record.trial.stale ? {...state, trial: {...record, trial: {...record.trial, stale: true}}} : state;
}

export function failRecognition(state:RecognitionState, token:string, error:Fault):RecognitionState {
  return state.owner.token === token ? {...state, error} : state;
}

export function clearRecognitionMessages(state:RecognitionState):RecognitionState {
  return state.notice === null && state.error === null ? state : {...state, notice: null, error: null};
}

// The host holds exactly the local draft, so a command naming `view.document_revision` acts on what is shown.
export function hostCurrent(state:RecognitionState):boolean {
  return state.document === null || sameJson(state.document, state.view.document);
}

function hasOtherPendingCrops(state:RecognitionState):boolean {
  for (const captureId in state.pendingCrops) {
    if (captureId !== state.view.capture_id && state.pendingCrops[captureId].ids.length > 0) return true;
  }
  return false;
}

function metadataDirty(state:RecognitionState):boolean {
  if (state.view.migration_required) return true;
  const captures = captureDocuments(state);
  return captures.length !== state.view.saved_captures.length || captures.some(capture => {
    const saved = state.view.saved_captures.find(item => item.capture_id === capture.capture_id);
    return !saved || !sameContent(capture.document, saved.document);
  });
}

// Pixel choices on any capture keep the global Edit session dirty, even after another capture is saved.
export function recognitionDirty(state:RecognitionState):boolean {
  return state.cropIds.length > 0 || hasOtherPendingCrops(state) || metadataDirty(state);
}

// Copy and frame trials require current confirmed setup, either explicit or reused on a compatible frame.
export function geometryConfirmed(state:RecognitionState):boolean {
  const {document, view} = state;
  return document !== null && view.frame !== null && view.frame.confirmed && sameBasis(document.basis, view.document?.basis);
}

// ---------------------------------------------------------------------------------------------------------------
// Local edits. Each returns the same state for a no-op and sets `notice` for a refused edit.

function record(state:RecognitionState, group:string|null, after:RecognitionDocument|null):Pick<RecognitionState, 'undo' | 'undoBytes' | 'group'> {
  if (group !== null && group === state.group) return {undo: state.undo, undoBytes: state.undoBytes, group};
  const document = state.document!;
  const capture_id = state.view.capture_id!;
  const crops = Object.fromEntries(state.cropIds
    .filter(id => state.cropSources[id] && !after?.definitions.some(definition => definition.id === id))
    .map(id => [id, {mark: state.cropMarks[id], source: state.cropSources[id]}]));
  const snapshot = {capture_id, document, selected: state.selected, crops};
  const entry: UndoEntry = {...snapshot, bytes: utf8Bytes(JSON.stringify(snapshot))};
  let undo = [...state.undo, entry];
  let undoBytes = state.undoBytes + entry.bytes;
  while (undo.length > 0 && (undo.length > UNDO_ENTRIES || undoBytes > UNDO_BYTES)) {
    undoBytes -= undo[0].bytes;
    undo = undo.slice(1);
  }
  return {undo, undoBytes, group};
}

function edit(state:RecognitionState, document:RecognitionDocument, group:string|null, selected:string|null = state.selected):RecognitionState {
  const before = state.document;
  if (before === null) return state;
  if (sameJson(before, document)) return selected === state.selected ? state : {...state, selected};
  if (packageBytes(state, document) > MAX_DOCUMENT_BYTES) return {...state, notice: 'documentLimit'};
  if (captureDocuments(state, document).reduce((count, capture) => count + capture.document.definitions.length, 0) > MAX_DEFINITIONS) return {...state, notice: 'definitionLimit'};
  let {clock} = state;
  const previous = new Map(before.definitions.map(definition => [definition.id, definition]));
  const definitions = document.definitions.map(definition => {
    const old = previous.get(definition.id);
    return old && sameJson(old, definition) ? old : {...definition, revision: ++clock};
  });
  const next = {...state, ...record(state, group, document), clock, document: {...document, definitions}, selected, localRevision: state.localRevision + 1, notice: null};
  return stamp(next, before);
}

function withDefinition(state:RecognitionState, id:string, change:(definition:RecognitionDefinition) => RecognitionDefinition, group:string|null):RecognitionState {
  const document = state.document;
  if (!document?.definitions.some(item => item.id === id)) return state;
  return edit(state, {...document, definitions: document.definitions.map(item => item.id === id ? change(item) : item)}, group);
}

export function selectDefinition(state:RecognitionState, id:string|null):RecognitionState {
  if (id !== null && !state.document?.definitions.some(definition => definition.id === id)) return state;
  return id === state.selected ? state : {...state, selected: id, group: null};
}

// Drag-to-create adds an OCR definition (the default kind) and selects it.
export function createDefinition(state:RecognitionState, region:NormalizedRect, name:string):RecognitionState {
  const document = state.document;
  if (!document) return state;
  if (aggregateDefinitions(state) >= MAX_DEFINITIONS) return {...state, notice: 'definitionLimit'};
  if (!mapRegion(region, document.basis)) return {...state, notice: 'invalidGeometry'};
  const ids = new Set(document.definitions.map(definition => definition.id));
  let number = state.nextId;
  while (ids.has(`r${number}`)) number += 1;
  const id = `r${number}`;
  const definition: RecognitionDefinition = {id, name, revision: 0, kind: 'ocr', region, expected: null, template: null, saved: null};
  const next = edit({...state, nextId: number + 1}, {...document, definitions: [...document.definitions, definition]}, null, id);
  return next.document === document ? {...next, nextId: state.nextId} : next;
}

export function setRegion(state:RecognitionState, id:string, part:'region' | 'search', region:NormalizedRect):RecognitionState {
  const document = state.document;
  const definition = document?.definitions.find(item => item.id === id);
  if (!document || !definition || (part === 'search' && definition.template === null)) return state;
  if (!mapRegion(region, document.basis)) return {...state, notice: 'invalidGeometry'};
  return withDefinition(state, id, item => part === 'region' ? {...item, region} : {...item, template: {...item.template!, search_region: region}}, null);
}

// Content selection keeps normalized definitions, so their frame rectangles follow the new content.
export function setContent(state:RecognitionState, content:PixelRect):RecognitionState {
  const document = state.document;
  if (!document) return state;
  const basis = {...document.basis, content};
  if (!validBasis(basis)) return {...state, notice: 'invalidGeometry'};
  const next = edit(state, {...document, basis}, null);
  return sameBasis(next.document?.basis, basis) ? setDisplay(next, {...next.display, tool: 'zones'}) : next;
}

// Different-sized frames keep the previous ROI until the author explicitly resets geometry.
export function rebaseFrame(state:RecognitionState):RecognitionState {
  const frame = state.view.frame;
  if (!frame || !state.document) return state;
  return edit(state, {...state.document, basis:fullBasis(frame)}, null);
}

export function renameDefinition(state:RecognitionState, id:string, name:string):RecognitionState {
  if (utf8Bytes(name) > MAX_NAME_BYTES || name.includes('\0')) return {...state, notice: 'nameLimit'};
  return withDefinition(state, id, item => ({...item, name}), `name:${id}`);
}

// Optional Script reference text; empty means unset. It never participates in a trial or a recognition request.
export function setExpected(state:RecognitionState, id:string, text:string):RecognitionState {
  if (utf8Bytes(text) > MAX_EXPECTED_BYTES) return {...state, notice: 'expectedLimit'};
  return withDefinition(state, id, item => ({...item, expected: text === '' ? null : text}), `expected:${id}`);
}

// Template recognition is an explicit choice; its search ROI starts at the whole content and uses upstream defaults.
export function setKind(state:RecognitionState, id:string, kind:RecognitionKind):RecognitionState {
  return withDefinition(state, id, item => item.kind === kind ? item : {...item, kind,
    template: kind === 'template' ? {search_region: {...FULL_REGION}, ...TEMPLATE_DEFAULTS} : null}, null);
}

// A wholly empty, unreviewed rights form is no rights draft: typing and clearing it leaves the document unchanged.
export function setRights(state:RecognitionState, rights:TemplateRights|null):RecognitionState {
  const document = state.document;
  const empty = rights !== null && rights.license === '' && rights.created_by === '' && (rights.created_for ?? '') === '' && !rights.reviewed;
  return document ? edit(state, {...document, template_rights: empty ? null : rights}, 'rights') : state;
}

export function deleteDefinition(state:RecognitionState, id:string):RecognitionState {
  const document = state.document;
  const index = document?.definitions.findIndex(item => item.id === id) ?? -1;
  if (!document || index < 0) return state;
  const definitions = document.definitions.filter(item => item.id !== id);
  const selected = state.selected === id ? definitions[Math.min(index, definitions.length - 1)]?.id ?? null : state.selected;
  return edit(state, {...document, definitions}, null, selected);
}

// Replaces the whole draft (Undo, Discard) and restamps it; revisions come from the replacement as they are, since
// a definition revision always names the same content.
function replaceDocument(state:RecognitionState, document:RecognitionDocument|null, selected:string|null):RecognitionState {
  const kept = selected !== null && document?.definitions.some(definition => definition.id === selected) ? selected : null;
  return stamp({...state, document, selected: kept, group: null, localRevision: state.localRevision + 1, notice: null}, state.document);
}

// Restores metadata, selection and any deleted crop whose original is still held by the host. Changed recognition
// inputs get new marks, so Undo never revives an earlier trial or Copy. Host-owned saved references and a basis for
// other frame dimensions are not restored.
export function undoRecognition(state:RecognitionState):RecognitionState {
  let index = state.undo.length - 1;
  while (index >= 0 && state.undo[index].capture_id !== state.view.capture_id) index -= 1;
  if (index < 0) return state;
  const entry = state.undo[index];
  const current = state.document;
  const live = new Map(current?.definitions.map(definition => [definition.id, definition.saved]) ?? []);
  const disk = new Map(state.view.saved_document?.definitions.map(definition => [definition.id, definition.saved]) ?? []);
  const definitions = entry.document.definitions.map(definition => {
    const saved = live.has(definition.id) ? live.get(definition.id)! : disk.get(definition.id) ?? null;
    return sameJson(saved, definition.saved) ? definition : {...definition, saved};
  });
  const resized = current && (entry.document.basis.frame_width !== current.basis.frame_width
    || entry.document.basis.frame_height !== current.basis.frame_height);
  const basis = resized ? current.basis : entry.document.basis;
  const document = {...entry.document, basis, definitions};
  if (packageBytes(state, document) > MAX_DOCUMENT_BYTES) return {...state, notice: 'documentLimit'};
  if (captureDocuments(state, document).reduce((count, capture) => count + capture.document.definitions.length, 0) > MAX_DEFINITIONS) return {...state, notice: 'definitionLimit'};
  const restored = Object.entries(entry.crops).filter(([id, crop]) => !live.has(id)
    && (crop.source === state.view.frame?.id
      || ((state.view.staged_crop_ids.includes(id) || state.view.stale_crop_ids.includes(id))
        && state.view.staged_crop_sources[id] === crop.source)));
  const next = {...state, undo: state.undo.filter((_, position) => position !== index), undoBytes: state.undoBytes - entry.bytes,
    cropIds: [...state.cropIds, ...restored.map(([id]) => id)],
    cropMarks: {...state.cropMarks, ...Object.fromEntries(restored.map(([id, crop]) => [id, crop.mark]))},
    cropSources: {...state.cropSources, ...Object.fromEntries(restored.map(([id, crop]) => [id, crop.source]))}};
  return replaceDocument(next, document, entry.selected);
}

export function toggleTrial(state:RecognitionState, id:string):RecognitionState {
  const document = state.document;
  if (!document?.definitions.some(item => item.id === id && item.kind === 'ocr')) return state;
  const chosen = new Set(state.trialIds);
  if (chosen.has(id)) chosen.delete(id); else chosen.add(id);
  return {...state, trialIds: document.definitions.map(item => item.id).filter(item => chosen.has(item)), trialSelection: state.trialSelection + 1};
}

export function toggleCrop(state:RecognitionState, id:string):RecognitionState {
  const document = state.document;
  if (!document?.definitions.some(item => item.id === id)) return state;
  const chosen = new Set(state.cropIds);
  const clock = chosen.has(id) ? state.clock : state.clock + 1;
  if (chosen.has(id)) chosen.delete(id); else chosen.add(id);
  const cropIds = document.definitions.map(item => item.id).filter(item => chosen.has(item));
  const cropMarks = Object.fromEntries(cropIds.map(item => [item, item === id ? clock : state.cropMarks[item]]));
  const cropSources = Object.fromEntries(cropIds.filter(item => item !== id && state.cropSources[item]).map(item => [item, state.cropSources[item]]));
  if (chosen.has(id) && state.view.frame) cropSources[id] = state.view.frame.id;
  return {...state, clock, cropIds, cropMarks, cropSources};
}

export function setDisplay(state:RecognitionState, display:PreviewDisplay):RecognitionState {
  const levels: readonly number[] = ZOOM_LEVELS;
  if ((display.zoom !== 'fit' && !levels.includes(display.zoom)) || (display.tool !== 'zones' && display.tool !== 'content')) return state;
  return sameJson(display, state.display) ? state : {...state, display};
}

// ---------------------------------------------------------------------------------------------------------------
// Host commands. A command handler first sends `syncTicket` (when non-null) through `recognition_update` and applies
// the reply with `applySync`; then it asks the command's block function and ticket. Tickets are null while the host
// does not hold the current draft or the command is blocked.

export function syncTicket(state:RecognitionState):SyncTicket|null {
  if (state.document === null || hostCurrent(state)) return null;
  if (state.view.capture_id === null) return null;
  return {capture_id: state.view.capture_id, token: state.owner.token, revision: state.view.revision, document_revision: state.view.document_revision,
    local_revision: state.localRevision, frame_id: state.view.frame?.id ?? null, document: state.document};
}

export function syncBlock(state:RecognitionState):RecognitionBlock|null {
  if (state.document === null) return 'noDocument';
  return documentValid(state.document) ? null : 'invalid';
}

export function applySync(state:RecognitionState, ticket:SyncTicket, view:RecognitionView):RecognitionState {
  return ticket.token === state.owner.token && ticket.capture_id === state.view.capture_id
    && ticket.document_revision === state.view.document_revision ? applyView({...state, error: null}, view, ticket.document) : state;
}

// Discard releases every staged original, including captures removed by the host. Keep newer local metadata, but
// no pixel choice or Undo entry may claim the released images.
export function applyDiscard(state:RecognitionState, view:RecognitionView, sent:RecognitionState = state):RecognitionState {
  if (view.owner.token !== state.owner.token || view.document_revision < state.view.document_revision
    || (view.capture_id !== state.view.capture_id && view.document_revision <= state.view.document_revision)
    || sent.owner.token !== state.owner.token || sent.view.capture_id !== state.view.capture_id
    || sent.view.document_revision !== state.view.document_revision) return state;
  const cleared = {...state, ...clearCropUndo(state), cropIds: [], cropMarks: {}, cropSources: {}, pendingCrops: {}, group: null, error: null};
  if (view.capture_id !== state.view.capture_id || state.localRevision !== sent.localRevision) return applyView(cleared, view, null);
  const target = hostDocument(view);
  const history = cleared.document !== null && !sameJson(cleared.document, target) ? record(cleared, null, target) : {};
  const next = replaceDocument({...cleared, ...history}, target, state.selected);
  return applyView(next, view, target);
}

export function confirmBlock(state:RecognitionState):RecognitionBlock|null {
  if (state.document === null) return 'noDocument';
  if (state.view.frame === null) return 'noFrame';
  if (state.running) return 'running';
  if (!documentValid(state.document)) return 'invalid';
  return geometryConfirmed(state) ? 'confirmed' : null;
}

export function confirmTicket(state:RecognitionState):ConfirmTicket|null {
  const frame = state.view.frame;
  if (confirmBlock(state) !== null || !hostCurrent(state) || frame === null) return null;
  if (state.view.capture_id === null) return null;
  return {capture_id: state.view.capture_id, token: state.owner.token, revision: state.view.revision, frame_id: frame.id, document_revision: state.view.document_revision};
}

export function applyConfirm(state:RecognitionState, ticket:ConfirmTicket, view:RecognitionView):RecognitionState {
  return ticket.token === state.owner.token && ticket.capture_id === state.view.capture_id
    && ticket.document_revision === state.view.document_revision ? applyView({...state, error: null}, view) : state;
}

// Grouped OCR (1..=engine bound, one request, document order) or exactly one template; a sample rechecks one
// saved OCR crop without the original frame. Saving more definitions than the bound stays valid.
export function trialBlock(state:RecognitionState, kind:'frame' | 'sample', ids:readonly string[]):RecognitionBlock|null {
  const document = state.document;
  if (document === null) return 'noDocument';
  if (state.running) return 'running';
  const limit = state.view.capabilities.max_ocr_zones;
  if (limit === null) return 'noCapability';
  const chosen = document.definitions.filter(item => ids.includes(item.id));
  if (chosen.length === 0 || chosen.length !== new Set(ids).size) return 'empty';
  if (kind === 'sample') return chosen.length === 1 && chosen[0].kind === 'ocr' && chosen[0].saved !== null ? null : 'noSample';
  if (state.view.frame === null) return 'noFrame';
  if (!geometryConfirmed(state)) return 'unconfirmed';
  if (chosen.some(item => item.kind !== chosen[0].kind)) return 'mixedKinds';
  if (chosen[0].kind === 'template' && chosen.length !== 1) return 'templateSingle';
  if (chosen[0].kind === 'ocr' && chosen.length > limit) return 'overLimit';
  return chosen.every(item => definitionIssue(item, document.basis) === null) ? null : 'invalid';
}

export function trialTicket(state:RecognitionState, kind:'frame' | 'sample', ids:readonly string[]):TrialTicket|null {
  const document = state.document;
  if (!document || trialBlock(state, kind, ids) !== null || !hostCurrent(state)) return null;
  if (state.view.capture_id === null) return null;
  const order = document.definitions.map(item => item.id).filter(id => ids.includes(id));
  const marks = Object.fromEntries(order.map(id => [id, state.marks[id]]));
  return {capture_id: state.view.capture_id, token: state.owner.token, revision: state.view.revision, kind, frame_id: kind === 'frame' ? state.view.frame!.id : null,
    document_revision: state.view.document_revision, selected_ids: order, sample_id: kind === 'sample' ? order[0] : null,
    stamp: {basis: state.basis, marks}};
}

export function beginTrial(state:RecognitionState, ticket:TrialTicket):RecognitionState {
  return ticket.token === state.owner.token && ticket.capture_id === state.view.capture_id ? {...state, running: ticket, error: null} : state;
}

// A settled trial or refusal stays attributed to its ticket's inputs, however late it arrives. A trial whose
// captured owner, revisions, frame or sample differ from the ticket is kept only as historical.
export function applyTrial(state:RecognitionState, ticket:TrialTicket, trial:RecognitionTrial|null, fault:Fault|null = null):RecognitionState {
  if (ticket.token !== state.owner.token || ticket.capture_id !== state.view.capture_id || ticket.document_revision !== state.view.document_revision) return state;
  const running = state.running === ticket ? null : state.running;
  const matches = trial === null || (trial.owner.token === ticket.token && trial.revision === ticket.revision && trial.document_revision === ticket.document_revision
    && trial.capture_id === ticket.capture_id
    && trial.frame_id === ticket.frame_id && trial.sample_id === ticket.sample_id);
  return {...state, running, trial: {ticket: matches ? ticket : null, trial, fault}};
}

// Whether the retained trial still describes `id` as it is now. Conservative: the host has not marked it stale,
// the host still holds exactly the draft and package revision the trial captured, and neither the definition's
// recognition inputs, the frame, the content basis nor the App OCR configuration changed.
export function trialFreshness(state:RecognitionState, id:string):Freshness {
  const record = state.trial;
  const ticket = record?.ticket;
  const trial = record?.trial;
  if (!ticket || !trial) return 'historical';
  const {view} = state;
  const mark = ticket.stamp.marks[id];
  if (trial.stale || trial.capture_id !== view.capture_id || ticket.capture_id !== view.capture_id || trial.owner.token !== state.owner.token || trial.revision !== view.revision || trial.document_revision !== view.document_revision
    || !hostCurrent(state) || trial.configuration_revision !== view.configuration_revision || mark === undefined || mark !== state.marks[id]) return 'stale';
  if (ticket.kind === 'frame') {
    const frame = view.frame;
    if (frame === null || trial.frame_id !== frame.id || trial.frame_revision !== frame.revision || ticket.stamp.basis !== state.basis || !geometryConfirmed(state)) return 'stale';
  }
  return 'fresh';
}

function basisReusable(state:RecognitionState):boolean {
  const document = state.document;
  return document === null || (state.view.basis_confirmed && sameBasis(document.basis, state.view.document?.basis))
    || sameBasis(document.basis, state.view.saved_document?.basis);
}

export function canReleaseImage(state:RecognitionState, leavingCapture:boolean):boolean {
  return (!leavingCapture && state.view.frame === null) || basisReusable(state);
}

export function saveBlock(state:RecognitionState):RecognitionBlock|null {
  const document = state.document;
  if (document === null) return 'noDocument';
  const staged = (id:string) => state.view.staged_crop_ids.includes(id)
    && state.view.staged_crop_sources[id] === state.cropSources[id] && !state.view.stale_crop_ids.includes(id);
  if (state.cropIds.some(id => !staged(id) && (!state.view.frame || state.cropSources[id] !== state.view.frame.id))) return 'cropFrame';
  if (state.cropIds.some(id => !staged(id)) && !geometryConfirmed(state)) return 'unconfirmed';
  if (!state.view.other_bases_confirmed || (!basisReusable(state)
    && !(state.cropIds.length > 0 && state.cropIds.every(staged)))) return 'unconfirmed';
  // Save is per capture: pixels pending on an inactive capture need that capture selected, not a global Save.
  if (state.cropIds.length === 0 && !metadataDirty(state)) return hasOtherPendingCrops(state) ? 'otherCapture' : 'noChanges';
  const savedTemplate = document.definitions.some(item => item.kind === 'template' && (item.saved !== null || state.cropIds.includes(item.id)));
  if (savedTemplate && !rightsValid(document.template_rights)) return 'rights';
  return null;
}

export function saveTicket(state:RecognitionState):SaveTicket|null {
  if (saveBlock(state) !== null || !hostCurrent(state) || state.document === null) return null;
  if (state.view.capture_id === null) return null;
  return {capture_id: state.view.capture_id, token: state.owner.token, revision: state.view.revision, document_revision: state.view.document_revision,
    crop_ids: [...state.cropIds], crop_sources: Object.fromEntries(state.cropIds.map(id => [id, state.cropSources[id]])), document: state.document,
    crops: Object.fromEntries(state.cropIds.map(id => [id, [state.cropMarks[id], state.marks[id]]]))};
}

// The commit is authoritative even when the follow-up read failed (`view` null). It releases this capture's stages.
// A submitted crop stays chosen when re-chosen or edited meanwhile, but Undo cannot reclaim its released original.
export function applySave(state:RecognitionState, ticket:SaveTicket, view:RecognitionView|null):RecognitionState {
  if (ticket.token !== state.owner.token || ticket.capture_id !== state.view.capture_id || ticket.document_revision !== state.view.document_revision) return state;
  const pending = state.cropIds.filter(id => {
    const submitted = ticket.crops[id];
    return !submitted || submitted[0] !== state.cropMarks[id] || submitted[1] !== state.marks[id];
  });
  const cleared = {...state, ...clearCropUndo(state, ticket.capture_id), error: null,
    view: {...state.view, staged_crop_ids: [], stale_crop_ids: [], staged_crop_sources: {}},
    pendingCrops: Object.fromEntries(Object.entries(state.pendingCrops).filter(([id]) => id !== ticket.capture_id)),
    cropIds: pending, cropMarks: Object.fromEntries(pending.map(id => [id, state.cropMarks[id]])),
    cropSources: Object.fromEntries(pending.map(id => [id, state.cropSources[id]]))};
  return view ? applyView(cleared, view, ticket.document) : cleared;
}

// A saved template's runtime Copy needs the whole draft to be the saved document (the host's check), with the
// template's PNG reference and reviewed rights.
export function templateSavedCurrent(state:RecognitionState, id:string):boolean {
  const definition = state.document?.definitions.find(item => item.id === id);
  const saved = state.view.saved_document;
  return definition !== undefined && definition.kind === 'template' && definition.saved !== null && saved !== null
    && sameContent(state.document, saved) && rightsValid(saved.template_rights);
}

// The definitions each Copy purpose names: none for the Game content setup, every checked OCR Trial row for grouped
// OCR (independent of Selected definition), and the Selected definition for a template.
function copySelection(state:RecognitionState, kind:SnippetKind):string[] {
  if (kind === 'game_content') return [];
  if (kind === 'ocr_recognize') return state.trialIds;
  return state.selected ? [state.selected] : [];
}

// Mirrors the host's whole-request validation; the host and its cached engine report stay authoritative. Grouped
// OCR needs the engine's reported bound and is refused above it, never split; setup and template Copy do not.
export function copyBlock(state:RecognitionState, ids:readonly string[], kind:SnippetKind):RecognitionBlock|null {
  const document = state.document;
  if (document === null) return 'noDocument';
  if (kind !== 'game_content' && ids.length === 0) return 'empty';
  if ((kind === 'game_content' && ids.length !== 0) || (kind === 'template_recognize' && ids.length !== 1) || new Set(ids).size !== ids.length) return 'invalid';
  const definitions = ids.map(id => document.definitions.find(item => item.id === id));
  if (!documentValid(document) || definitions.some(item => item === undefined)) return 'invalid';
  if (state.view.frame === null) return 'noFrame';
  if (!geometryConfirmed(state)) return 'unconfirmed';
  if (definitions.some(item => (kind === 'template_recognize') !== (item!.kind === 'template'))) return 'kind';
  if (kind === 'ocr_recognize') {
    const limit = state.view.capabilities.max_ocr_zones;
    if (limit === null) return 'noCapability';
    if (ids.length > limit) return 'overLimit';
  }
  if (kind === 'template_recognize' && !templateSavedCurrent(state, ids[0])) return 'templateUnsaved';
  return null;
}

export function copyTicket(state:RecognitionState, ids:readonly string[], kind:SnippetKind):CopyTicket|null {
  if (copyBlock(state, ids, kind) !== null || !hostCurrent(state) || !sameJson(ids, copySelection(state, kind))) return null;
  if (state.view.capture_id === null) return null;
  return {capture_id: state.view.capture_id, token: state.owner.token, revision: state.view.revision, document_revision: state.view.document_revision, definition_ids: [...ids], kind,
    stamp: {basis: state.basis, definitions: Object.fromEntries(ids.map(id => {
      const definition = state.document!.definitions.find(item => item.id === id)!;
      return [id, {revision: definition.revision, mark: (kind === 'ocr_recognize' ? state.sourceMarks : state.marks)[id]}];
    })), source: kind === 'template_recognize' ? state.view.revision : null, selection: state.trialSelection}};
}

// `result` is the host's complete generated source envelope; `error` its refusal or the clipboard failure.
export function applyCopy(state:RecognitionState, ticket:CopyTicket, result:CopyResult|null, error:Fault|null):RecognitionState {
  if (ticket.token !== state.owner.token || ticket.capture_id !== state.view.capture_id || ticket.document_revision !== state.view.document_revision
    || (result !== null && (result.capture_id !== ticket.capture_id || result.document_revision !== ticket.document_revision))) return state;
  const copy: CopyRecord = {capture_id: ticket.capture_id, definition_ids: result?.definition_ids ?? ticket.definition_ids, stamp: ticket.stamp,
    basis: result?.basis ?? null, verified: result?.verified ?? false, error};
  return {...state, copies: {...state.copies, [ticket.kind]: copy}};
}

// Whether published source still equals what the same purpose would copy now. Setup follows its basis value. Grouped
// OCR follows its checked set and zone definitions (not their saved diagnostic crops), not Game content, because it
// reads the pasted `recognitionBasis`. A template Copy is bound to the frame, content basis and saved package revision.
export function copyFreshness(state:RecognitionState, kind:SnippetKind):CopyFreshness|null {
  const copy = state.copies[kind];
  if (!copy) return null;
  if (copy.capture_id !== state.view.capture_id) return 'obsolete';
  if (copy.error) return 'failed';
  if (kind === 'game_content') return sameBasis(copy.basis, state.document?.basis) ? 'current' : 'obsolete';
  // Grouped OCR source ignores saved diagnostic crops; a template Copy carries its saved PNG.
  const marks = kind === 'ocr_recognize' ? state.sourceMarks : state.marks;
  const changed = !sameJson(copy.definition_ids, copySelection(state, kind)) || copy.definition_ids.some(id => {
    const definition = state.document?.definitions.find(item => item.id === id);
    const captured = copy.stamp.definitions[id];
    return !definition || !captured || definition.revision !== captured.revision || marks[id] !== captured.mark;
  });
  const obsolete = changed || (kind === 'ocr_recognize' ? copy.stamp.selection !== state.trialSelection
    : state.basis !== copy.stamp.basis || copy.stamp.source !== state.view.revision);
  return obsolete ? 'obsolete' : 'current';
}

// ---------------------------------------------------------------------------------------------------------------
// Detached preview window protocol. The main window emits PREVIEW_STATE to PREVIEW_LABEL whenever the snapshot
// changes and on PREVIEW_READY; the preview emits PREVIEW_EDIT messages the main window applies with
// `applyPreviewEdit`. The preview reads its raster itself through `recognition_preview`; no image data is relayed.

// A saved locator is not capture authority: Start asks the host to validate it against live windows.
export function nativePrimaryAction(selection:NativeSelectionView|null):'select'|'start'|'capture' {
  if (selection?.status === 'selected' && selection.selected_id !== null) return 'capture';
  return selection?.has_saved_target ? 'start' : 'select';
}

export const PREVIEW_LABEL = 'recognition-preview';
export const PREVIEW_SURFACE = 'recognition-preview';
export const PREVIEW_READY = 'recognition-preview-ready';
export const PREVIEW_STATE = 'recognition-preview-state';
export const PREVIEW_EDIT = 'recognition-preview-edit';
export const PREVIEW_ACTION = 'recognition-preview-action';

export interface PreviewStateMessage {publication:number; snapshot:PreviewSnapshot|null}

export interface PreviewCloseMessage {owner:AuthoringRef; revision:string; generation:number}
export interface PreviewCloseFailure extends PreviewCloseMessage {error:Fault}
export interface PreviewDefinition {
  id:string; name:string; kind:RecognitionKind; revision:number; region:NormalizedRect; search:NormalizedRect|null;
}
// Observed boxes of the retained frame trial in frame pixels, for on-image display only.
export interface PreviewObservation {id:string; bounds:PixelRect; fresh:boolean}
export interface PreviewSnapshot {
  owner:AuthoringRef; revision:string; locale:Locale; editable:boolean; lockReason:string|null;
  capture_id:string|null;
  localRevision:number; documentRevision:number; basisRevision:number;
  nativeSelection:NativeSelectionView|null; nativeCache:CaptureCacheReceipt|null; commandBusy:boolean;
  frame:RecognitionFrame|null; basis:GeometryBasis|null; definitions:PreviewDefinition[]; selected:string|null;
  confirmed:boolean; canUndo:boolean; canCreate:boolean; frameGeometryReady:boolean; display:PreviewDisplay; observations:PreviewObservation[];
  // The latest local refusal, so the preview can explain an edit it relayed that did not apply.
  notice:RecognitionNotice|null; error:Fault|null;
  // An authoring child holds the work reservation; the preview keeps the independent Stop reachable.
  running:boolean;
}
export type PreviewEdit =
  | {kind:'select'; id:string|null}
  | {kind:'display'; display:PreviewDisplay}
  | {kind:'create'; region:NormalizedRect}
  | {kind:'region'; id:string; part:'region' | 'search'; revision:number; region:NormalizedRect}
  | {kind:'content'; content:PixelRect}
  | {kind:'delete'; id:string; revision:number}
  | {kind:'undo'; localRevision:number}
  | {kind:'rebase'};
export type PreviewAction = 'select'|'start'|'capture'|'newCapture'|'cancel';
export interface PreviewActionMessage {
  token:string; revision:string; capture_id:string|null; documentRevision:number; localRevision:number; generation:number; action:PreviewAction;
}
// `frameId` and `basisRevision` are the snapshot the preview edited on; geometry made for another frame or
// content basis never applies.
export interface PreviewEditMessage {token:string; capture_id:string|null; revision:string; frameId:string|null; basisRevision:number; edit:PreviewEdit}

function observations(state:RecognitionState):PreviewObservation[] {
  const trial = state.trial?.trial;
  const result = trial && !trial.sample_id ? trialEnvelope(trial)?.result : null;
  if (!result) return [];
  if (result.kind === 'template') {
    const fresh = trialFreshness(state, result.id) === 'fresh';
    return result.matches.map(match => ({id: result.id, bounds: match.bounds, fresh}));
  }
  return result.zones.flatMap(zone => {
    const fresh = trialFreshness(state, zone.id) === 'fresh';
    return zone.regions.map(region => ({id: zone.id, bounds: region.bounds, fresh}));
  });
}

// `running` is the host controller's view of the authoring child (the local trial ticket is only a lower bound).
export function previewSnapshot(state:RecognitionState, locale:Locale, editable:boolean, lockReason:string|null, running:boolean = state.running !== null,
  nativeSelection:NativeSelectionView|null = null, nativeCache:CaptureCacheReceipt|null = null, commandBusy:boolean = false):PreviewSnapshot {
  const document = state.document;
  return {
    owner: state.owner, capture_id: state.view.capture_id, revision: state.view.revision, locale, editable, lockReason,
    localRevision: state.localRevision, documentRevision: state.view.document_revision, basisRevision: state.basis,
    nativeSelection, nativeCache, commandBusy,
    frame: state.view.frame, basis: document?.basis ?? null,
    definitions: document?.definitions.map(item => ({id: item.id, name: item.name, kind: item.kind, revision: item.revision,
      region: item.region, search: item.template?.search_region ?? null})) ?? [],
    selected: state.selected, confirmed: geometryConfirmed(state), canUndo: canUndoRecognition(state),
    canCreate: document !== null && aggregateDefinitions(state) < MAX_DEFINITIONS,
    frameGeometryReady: state.view.frame === null || (document !== null && document.basis.frame_width === state.view.frame.width
      && document.basis.frame_height === state.view.frame.height),
    display: state.display, observations: observations(state),
    notice: state.notice, error: state.error, running,
  };
}

// Applies one relayed on-image edit. Metadata edits are fenced by the frame and basis stamp of the edited snapshot,
// geometry and deletion also by the definition revision, and Undo by the local revision, so an edit made on an
// older snapshot is refused (`staleEdit`) instead of overwriting newer main-window changes.
export function applyPreviewEdit(state:RecognitionState, message:PreviewEditMessage, defaultName:(number:number) => string):RecognitionState {
  if (message.token !== state.owner.token || message.capture_id !== state.view.capture_id || message.revision !== state.view.revision) return state;
  const {edit: change} = message;
  const stale = {...state, notice: 'staleEdit' as const};
  if (message.frameId !== (state.view.frame?.id ?? null) || message.basisRevision !== state.basis) return stale;
  if (change.kind === 'select') return selectDefinition(state, change.id);
  if (change.kind === 'display') return setDisplay(state, change.display);
  const revision = (id:string) => state.document?.definitions.find(item => item.id === id)?.revision;
  switch (change.kind) {
    case 'create': return createDefinition(state, change.region, defaultName(state.nextId));
    case 'region': return revision(change.id) === change.revision ? setRegion(state, change.id, change.part, change.region) : stale;
    case 'content': return setContent(state, change.content);
    case 'rebase': return rebaseFrame(state);
    case 'delete': return revision(change.id) === change.revision ? deleteDefinition(state, change.id) : stale;
    case 'undo': return change.localRevision === state.localRevision ? undoRecognition(state) : stale;
  }
}

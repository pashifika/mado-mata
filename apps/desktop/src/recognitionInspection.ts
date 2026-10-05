import {clientToFrame, sameJson} from './recognition.ts';
import type {Point, PreviewDisplay, PreviewSnapshot} from './recognition.ts';
import type {AuthoringRef, Fault} from './types.ts';

export interface PixelRequest {
  owner:AuthoringRef; captureId:string; frameId:string; frameRevision:number; x:number; y:number;
}
export interface PixelSample {
  owner:AuthoringRef; capture_id:string; frame_id:string; frame_revision:number; x:number; y:number;
  rgba:[number, number, number, number];
}
export interface InspectionState {point:Point|null; sample:PixelSample|null; pending:boolean; error:Fault|null}
export interface RenderedRect {left:number; top:number; width:number; height:number}

// Reject the half-open boundary before flooring. The rendered rectangle already includes scroll and CSS scale.
export function clientToPixel(clientX:number, clientY:number, rect:RenderedRect, width:number, height:number):Point|null {
  if (![clientX, clientY, rect.left, rect.top, rect.width, rect.height, width, height].every(Number.isFinite)
    || rect.width <= 0 || rect.height <= 0 || !Number.isSafeInteger(width) || !Number.isSafeInteger(height)
    || width <= 0 || height <= 0) return null;
  // Test CSS edges directly: subtraction in the transform can round an exact right/bottom edge inward.
  if (clientX < rect.left || clientY < rect.top || clientX >= rect.left + rect.width || clientY >= rect.top + rect.height) return null;
  const point = clientToFrame(clientX, clientY, rect, width, height);
  return point.x >= 0 && point.y >= 0 && point.x < width && point.y < height
    ? {x:Math.floor(point.x), y:Math.floor(point.y)} : null;
}

export function pixelCenter(point:Point):Point { return {x:point.x + 0.5, y:point.y + 0.5}; }
export function rgbHex(rgba:PixelSample['rgba']):string {
  return `#${rgba.slice(0, 3).map(value => value.toString(16).padStart(2, '0')).join('').toUpperCase()}`;
}

function sourceIdentity(snapshot:PreviewSnapshot|null):string {
  return JSON.stringify(snapshot && [snapshot.owner.token, snapshot.owner.workspace.workspace_id, snapshot.owner.workspace.revision,
    snapshot.capture_id, snapshot.frame?.id, snapshot.frame?.revision, snapshot.frame?.width, snapshot.frame?.height]);
}

// One occupied invocation survives invalidation until real settlement. Only the replaceable desired selection
// is queued; no raster, document, history or recognition evidence is retained here.
export class PixelInspection {
  private snapshot:PreviewSnapshot|null = null;
  private display:PreviewDisplay = {zoom:'fit', tool:'zones'};
  private source = sourceIdentity(null);
  private raster:string|null = null;
  private rasterReady = false;
  private live = true;
  private epoch = 0;
  private generation = 0;
  private occupied = false;
  private desired:{request:PixelRequest; epoch:number; generation:number}|null = null;
  private state:InspectionState = {point:null, sample:null, pending:false, error:null};
  private read:(request:PixelRequest) => Promise<PixelSample>;
  private changed:(state:InspectionState) => void;
  private failure:(cause:unknown) => Fault;

  constructor(read:(request:PixelRequest) => Promise<PixelSample>, changed:(state:InspectionState) => void,
    failure:(cause:unknown) => Fault) {
    this.read = read;
    this.changed = changed;
    this.failure = failure;
  }

  get value():InspectionState { return this.state; }
  get sourceEpoch():number { return this.epoch; }
  // Event handlers can outlive the tool prop from their last React render.
  get inspecting():boolean { return this.display.tool === 'inspect'; }
  get available():boolean {
    return this.live && this.inspecting && this.rasterReady && Boolean(this.snapshot?.frame && this.snapshot.capture_id)
      && !this.snapshot?.nativeSelection?.busy;
  }

  // Called for every accepted incoming message, not a React effect: A -> B -> A counts twice.
  incoming(snapshot:PreviewSnapshot|null):PreviewDisplay {
    const source = sourceIdentity(snapshot);
    const sourceChanged = source !== this.source;
    // Repeated main-window polls must not undo an optimistic local tool/zoom edit.
    const display:PreviewDisplay = sourceChanged || !sameJson(snapshot?.display, this.snapshot?.display)
      ? snapshot?.display ?? {zoom:'fit', tool:'zones'} : this.display;
    const toolChanged = display.tool !== this.display.tool;
    const acquisitionStarted = Boolean(snapshot?.nativeSelection?.busy && !this.snapshot?.nativeSelection?.busy);
    this.snapshot = snapshot;
    if (sourceChanged) {
      this.source = source;
      this.epoch++;
      this.raster = null;
      this.rasterReady = false;
    }
    this.display = display;
    if (sourceChanged || acquisitionStarted || (toolChanged && !this.inspecting) || !snapshot?.frame || !snapshot.capture_id) this.clear();
    return display;
  }

  setDisplay(display:PreviewDisplay):void {
    if (display.tool !== this.display.tool) this.clear();
    this.display = display;
  }

  // The blob URL identifies this particular image element, not just an equal-sized frame.
  setRaster(epoch:number, image:string|null):void {
    if (epoch !== this.epoch || !this.live || image === this.raster) return;
    this.raster = image;
    this.rasterReady = false;
    this.clear();
  }

  rasterLoaded(epoch:number, image:string):boolean {
    if (!this.live || epoch !== this.epoch || image !== this.raster) return false;
    this.rasterReady = true;
    return true;
  }

  rasterFailed(epoch:number, image:string):boolean {
    if (!this.live || epoch !== this.epoch || image !== this.raster) return false;
    this.raster = null;
    this.rasterReady = false;
    this.clear();
    return true;
  }

  open():void {
    if (this.live) return;
    this.live = true;
    this.clear();
  }
  close():void {
    if (!this.live) return;
    this.epoch++;
    this.raster = null;
    this.rasterReady = false;
    this.clear();
    this.live = false;
  }

  clear():void {
    this.generation++;
    this.desired = null;
    this.publish({point:null, sample:null, pending:false, error:null});
  }

  select(point:Point):boolean {
    const frame = this.snapshot?.frame;
    if (!this.available || !frame || !Number.isSafeInteger(point.x) || !Number.isSafeInteger(point.y)
      || point.x < 0 || point.y < 0 || point.x >= frame.width || point.y >= frame.height) return false;
    const request:PixelRequest = {owner:this.snapshot!.owner, captureId:this.snapshot!.capture_id!, frameId:frame.id,
      frameRevision:frame.revision, x:point.x, y:point.y};
    this.desired = {request, epoch:this.epoch, generation:++this.generation};
    this.publish({point:{...point}, sample:null, pending:true, error:null});
    this.dispatch();
    return true;
  }

  pointer(clientX:number, clientY:number, rect:RenderedRect):boolean {
    const frame = this.snapshot?.frame;
    const point = frame && clientToPixel(clientX, clientY, rect, frame.width, frame.height);
    return Boolean(point && this.select(point));
  }

  // Only the surface itself owns these keys. Shift never changes the one-original-pixel step.
  key(key:string, focused:boolean):boolean {
    if (!focused || !this.available || !this.state.point) return false;
    if (key === 'Escape') { this.clear(); return true; }
    const offset:Record<string, [number, number]> = {ArrowLeft:[-1, 0], ArrowRight:[1, 0], ArrowUp:[0, -1], ArrowDown:[0, 1]};
    const delta = offset[key];
    if (!delta) return false;
    this.select({x:this.state.point.x + delta[0], y:this.state.point.y + delta[1]});
    // At an edge this is still a focused pixel-step key, not a viewport scroll command.
    return true;
  }

  private publish(state:InspectionState):void {
    this.state = state;
    if (this.live) this.changed(state);
  }

  private current(ticket:NonNullable<PixelInspection['desired']>):boolean {
    const frame = this.snapshot?.frame, request = ticket.request;
    return this.available && ticket.epoch === this.epoch && ticket.generation === this.generation
      && sameJson(request.owner, this.snapshot?.owner) && request.captureId === this.snapshot?.capture_id
      && request.frameId === frame?.id && request.frameRevision === frame?.revision
      && request.x === this.state.point?.x && request.y === this.state.point?.y;
  }

  private dispatch():void {
    if (this.occupied || !this.desired || !this.available) return;
    const ticket = this.desired;
    this.desired = null;
    this.occupied = true;
    // Promise.resolve also turns a synchronous transport failure into normal settlement.
    void Promise.resolve().then(() => this.current(ticket) ? this.read(ticket.request) : null).then(sample => {
      const request = ticket.request;
      if (sample === null || !this.current(ticket)) return;
      if (!sameJson(sample.owner, request.owner) || sample.capture_id !== request.captureId || sample.frame_id !== request.frameId
        || sample.frame_revision !== request.frameRevision || sample.x !== request.x || sample.y !== request.y) {
        this.publish({point:this.state.point, sample:null, pending:false,
          error:{category:'Stale', message:'The pixel response does not match the selected original frame.', context:null}});
        return;
      }
      this.publish({point:this.state.point, sample, pending:false, error:null});
    }, cause => {
      if (this.current(ticket)) this.publish({point:this.state.point, sample:null, pending:false, error:this.failure(cause)});
    }).finally(() => {
      this.occupied = false;
      this.dispatch();
    });
  }
}

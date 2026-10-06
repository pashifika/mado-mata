import type {PixelRect} from './recognition.ts';

// Same ceiling as the host's display raster (images::CROP_MAX_PIXELS), not the original frame.
// No OCR, native capture, image resizing or ratio-driven center crop occurs here.
export const CONTENT_MAX_PIXELS = 4_194_304;
const MAX_SIDE = 16_384;
const MAX_FRAME_PIXELS = 16_777_216;
export const CONTENT_ASPECTS = ['16:9', '16:10', '4:3'] as const;
export type ContentAspect = 'auto' | typeof CONTENT_ASPECTS[number];
export interface ContentPixels {width:number; height:number; data:Uint8ClampedArray}
export interface ContentProposal {rect:PixelRect; aspect:typeof CONTENT_ASPECTS[number]; approximate:boolean}
export type ContentDetection = {kind:'candidate'; proposal:ContentProposal} | {kind:'none' | 'ambiguous'};
interface Edge {position:number; support:number}
interface Ranked {rect:PixelRect; aspect:typeof CONTENT_ASPECTS[number]; score:number}

const SAMPLES = 96;
const EDGE_LIMIT = 8;
const RATIOS = {'16:9': 16 / 9, '16:10': 16 / 10, '4:3': 4 / 3};

function positive(value:number):boolean { return Number.isSafeInteger(value) && value > 0 && value <= MAX_SIDE; }

function pixel(image:ContentPixels, x:number, y:number, out:Uint8Array, offset:number) {
  const index = (y * image.width + x) * 4;
  const alpha = image.data[index + 3];
  // Hidden RGB in transparent pixels must not invent edges or texture.
  for (let channel = 0; channel < 3; channel++) out[offset + channel] = Math.round(image.data[index + channel] * alpha / 255);
  out[offset + 3] = alpha;
}

function median(samples:Uint8Array, count:number, scratch:Uint16Array):number[] {
  scratch.fill(0);
  for (let i = 0; i < count * 4; i++) scratch[(i % 4) * 256 + samples[i]]++;
  const result:number[] = [];
  for (let channel = 0; channel < 4; channel++) {
    let total = 0, value = 0;
    for (; value < 255; value++) { total += scratch[channel * 256 + value]; if (total > count / 2) break; }
    result.push(value);
  }
  return result;
}

function distance(samples:Uint8Array, offset:number, reference:number[]):number {
  let delta = 0;
  for (let channel = 0; channel < 4; channel++) delta = Math.max(delta, Math.abs(samples[offset + channel] - reference[channel]));
  return delta;
}

// Every tested boundary has an observed outside scanline and an adjacent inside scanline.
// Sample along the central 88% of each line: title icons/corner antialiasing may be outliers.
// Stop after a non-flat exterior, rather than skipping over game pixels to find an inner UI panel.
function edges(image:ContentPixels, horizontal:boolean, reverse:boolean):Edge[] {
  const length = horizontal ? image.height : image.width;
  const across = horizontal ? image.width : image.height;
  const count = Math.min(SAMPLES, across);
  const positions = Array.from({length: count}, (_, i) => Math.min(across - 1, Math.floor(across * (0.06 + 0.88 * (i + 0.5) / count))));
  let outside = new Uint8Array(count * 4), inside = new Uint8Array(count * 4);
  const scratch = new Uint16Array(1024);
  function read(at:number, output:Uint8Array) {
    positions.forEach((position, i) => pixel(image, horizontal ? position : at, horizontal ? at : position, output, i * 4));
  }
  const candidates:Edge[] = [];
  let nonFlat = 0;
  read(reverse ? length - 1 : 0, outside);
  for (let depth = 0; depth < Math.floor(length / 4); depth++) {
    const at = reverse ? length - 2 - depth : depth + 1;
    read(at, inside);
    const color = median(outside, count, scratch);
    let flat = 0, changed = 0;
    for (let i = 0; i < count; i++) {
      if (distance(outside, i * 4, color) <= 8) flat++;
      let delta = 0;
      for (let channel = 0; channel < 4; channel++) delta = Math.max(delta, Math.abs(outside[i * 4 + channel] - inside[i * 4 + channel]));
      if (delta > 8) changed++;
    }
    const flatness = flat / count, continuity = changed / count;
    if (flatness < 0.75) nonFlat++;
    if (nonFlat > Math.max(2, Math.floor((depth + 1) * 0.15))) break;
    if (flatness >= 0.75 && continuity >= 0.65) {
      candidates.push({position: reverse ? at + 1 : at, support: (flatness + continuity) / 2});
    }
    [outside, inside] = [inside, outside];
  }
  // Keep a bounded set of observed transitions. Adjacent shadow/frame edges remain
  // available to ratio scoring; neighbouring final rectangles are not independent alternatives.
  const selected:Edge[] = [];
  for (const edge of candidates.sort((a, b) => b.support - a.support || (reverse ? b.position - a.position : a.position - b.position))) {
    if (selected.every(other => Math.abs(other.position - edge.position) > 0)) selected.push(edge);
    if (selected.length === EDGE_LIMIT - 1) break;
  }
  return [{position: reverse ? length : 0, support: 0}, ...selected];
}

function textured(image:ContentPixels, rect:PixelRect):boolean {
  const count = 16 * 12, samples = new Uint8Array(count * 4);
  for (let y = 0; y < 12; y++) for (let x = 0; x < 16; x++) {
    pixel(image, rect.x + Math.min(rect.width - 1, Math.floor((x + 0.5) * rect.width / 16)),
      rect.y + Math.min(rect.height - 1, Math.floor((y + 0.5) * rect.height / 12)), samples, (y * 16 + x) * 4);
  }
  const color = median(samples, count, new Uint16Array(1024));
  let different = 0, visible = 0;
  for (let i = 0; i < count; i++) {
    if (samples[i * 4 + 3] >= 32) { visible++; if (distance(samples, i * 4, color) > 16) different++; }
  }
  return visible >= count * 0.8 && different >= count * 0.15;
}

function neighbouring(a:PixelRect, b:PixelRect):boolean {
  return Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y),
    Math.abs(a.x + a.width - b.x - b.width), Math.abs(a.y + a.height - b.y - b.height)) <= 2;
}

// Convert half-open raster edges, not CSS pixels or devicePixelRatio. A downsampled raster
// cannot prove a single-original-pixel boundary; the UI MUST label that result approximate.
export function contentToFrame(rect:PixelRect, rasterWidth:number, rasterHeight:number, width:number, height:number):PixelRect {
  if (![rasterWidth, rasterHeight, width, height].every(positive) || width < rasterWidth || height < rasterHeight
    || width * height > MAX_FRAME_PIXELS
    || ![rect.x, rect.y, rect.width, rect.height].every(Number.isSafeInteger)
    || rect.x < 0 || rect.y < 0 || rect.width <= 0 || rect.height <= 0
    || rect.x + rect.width > rasterWidth || rect.y + rect.height > rasterHeight) throw new Error('Invalid content geometry');
  const x = Math.floor(rect.x * width / rasterWidth), y = Math.floor(rect.y * height / rasterHeight);
  const right = Math.ceil((rect.x + rect.width) * width / rasterWidth);
  const bottom = Math.ceil((rect.y + rect.height) * height / rasterHeight);
  return {x, y, width: right - x, height: bottom - y};
}

export function detectContent(image:ContentPixels, aspect:ContentAspect = 'auto',
  frameWidth = image.width, frameHeight = image.height):ContentDetection {
  const {width, height, data} = image;
  if (![width, height, frameWidth, frameHeight].every(positive) || width * height > CONTENT_MAX_PIXELS
    || frameWidth < width || frameHeight < height || frameWidth * frameHeight > MAX_FRAME_PIXELS || !(data instanceof Uint8ClampedArray) || data.length !== width * height * 4
    || (aspect !== 'auto' && !CONTENT_ASPECTS.includes(aspect))) throw new Error('Invalid content detection input');
  if (width < 32 || height < 32) return {kind: 'none'};
  const ratios = aspect === 'auto' ? CONTENT_ASPECTS : [aspect];
  const left = edges(image, false, false), right = edges(image, false, true);
  const top = edges(image, true, false), bottom = edges(image, true, true);
  const sx = frameWidth / width, sy = frameHeight / height;
  const ranked:Ranked[] = [];
  for (const l of left) for (const r of right) for (const t of top) for (const b of bottom) {
    const rect = {x: l.position, y: t.position, width: r.position - l.position, height: b.position - t.position};
    if (rect.width < 32 || rect.height < 32) continue;
    const area = rect.width * rect.height / (width * height);
    const moved = [l, r, t, b].filter(edge => edge.support > 0);
    for (const ratio of ratios) {
      // At most two raster-pixel edge-rounding units. Never manufacture an edge to fit a ratio.
      const error = Math.abs(rect.width * sx - rect.height * sy * RATIOS[ratio]) / Math.max(sx, sy * RATIOS[ratio]);
      if (error > 2) continue;
      if (!textured(image, rect)) continue;
      const support = moved.reduce((sum, edge) => sum + edge.support, 0) / Math.max(1, moved.length);
      const score = moved.length ? 0.6 + support * 0.25 + moved.length * 0.025 + area * 0.12 + (1 - error / 2) * 0.02 : 0.6;
      ranked.push({rect, aspect: ratio, score});
    }
  }
  ranked.sort((a, b) => b.score - a.score || b.rect.width * b.rect.height - a.rect.width * a.rect.height);
  const best = ranked[0];
  if (!best) return {kind: 'none'};
  const alternative = ranked.find(item => item.aspect !== best.aspect || !neighbouring(item.rect, best.rect));
  if (alternative && best.score - alternative.score < 0.04) return {kind: 'ambiguous'};
  return {kind: 'candidate', proposal: {rect: contentToFrame(best.rect, width, height, frameWidth, frameHeight), aspect: best.aspect,
    approximate: width !== frameWidth || height !== frameHeight}};
}

// Borrow the already displayed image. The host's preview reservation accounts for the
// additional canvas backing store and ImageData below. Never decode another full original.
export function detectDisplayedContent(element:HTMLImageElement | null, url:string, aspect:ContentAspect,
  width:number, height:number):ContentDetection {
  if (!element || !element.complete || (element.currentSrc || element.src) !== url) throw new Error('Preview image is not ready');
  const w = element.naturalWidth, h = element.naturalHeight;
  if (!positive(w) || !positive(h) || w * h > CONTENT_MAX_PIXELS || w > width || h > height) throw new Error('Preview exceeds detection limit');
  const canvas = document.createElement('canvas');
  try {
    canvas.width = w;
    canvas.height = h;
    const context = canvas.getContext('2d', {willReadFrequently: true});
    if (!context) throw new Error('Canvas readback is unavailable');
    context.drawImage(element, 0, 0);
    return detectContent(context.getImageData(0, 0, w, h), aspect, width, height);
  } finally {
    canvas.width = canvas.height = 0;
  }
}

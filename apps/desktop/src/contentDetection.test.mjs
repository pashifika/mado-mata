import assert from 'node:assert/strict';
import {test} from 'node:test';
import {CONTENT_MAX_PIXELS, contentToFrame, detectContent, detectDisplayedContent} from './contentDetection.ts';

// Synthetic pixels only: no private screenshots, game assets, OCR text or native access.
function image(width, height, fill = [0, 0, 0, 255]) {
  const data = new Uint8ClampedArray(width * height * 4);
  for (let i = 0; i < data.length; i += 4) data.set(fill, i);
  return {width, height, data};
}
function paint(img, rect, color = null) {
  for (let y = rect.y; y < rect.y + rect.height; y++) for (let x = rect.x; x < rect.x + rect.width; x++) {
    img.data.set(color ?? [40 + (x * 37 + y * 13) % 170, 30 + (x * 17 + y * 41) % 190, 80 + (x * 23 + y * 7) % 140, 255], (y * img.width + x) * 4);
  }
  return img;
}
function windowImage(width, height, {left = 2, top = 28, right = 2, bottom = 2, shade = 242} = {}) {
  const rect = {x: left, y: top, width, height};
  const img = image(width + left + right, height + top + bottom, [shade, shade, shade, 255]);
  paint(img, rect);
  return {img, rect};
}
function candidate(img, rect, ratio = 'auto') {
  const result = detectContent(img, ratio);
  assert.equal(result.kind, 'candidate', JSON.stringify(result));
  assert.deepEqual(result.proposal.rect, rect);
  assert.equal(result.proposal.approximate, false);
  return result.proposal;
}

for (const [ratio, width, height] of [['16:9', 640, 360], ['16:10', 640, 400], ['4:3', 640, 480]]) {
  for (const shade of [0, 28, 242]) {
    test(`${ratio}: ${shade === 242 ? 'light' : 'dark'} title bar, no fixed title height (${shade})`, () => {
      const {img, rect} = windowImage(width, height, {top: shade === 28 ? 43 : 28, shade});
      assert.equal(candidate(img, rect).aspect, ratio);
      assert.equal(candidate(img, rect, ratio).aspect, ratio);
    });
  }
  test(`${ratio}: already borderless`, () => {
    const rect = {x: 0, y: 0, width, height};
    assert.equal(candidate(paint(image(width, height), rect), rect).aspect, ratio);
  });
}

test('title text and control icons are limited scanline outliers, not a reason to use a fixed inset', () => {
  const {img, rect} = windowImage(800, 450, {top: 38, shade: 244});
  paint(img, {x: 25, y: 8, width: 45, height: 14}, [60, 60, 60, 255]);
  paint(img, {x: 752, y: 7, width: 30, height: 12}, [60, 60, 60, 255]);
  candidate(img, rect);
});

test('16:9 letterboxing inside a 16:10 raster uses observed boundaries instead of the whole raster', () => {
  const rect = {x: 0, y: 20, width: 640, height: 360};
  candidate(paint(image(640, 400), rect), rect);
});

test('light pillarboxes can bound 4:3 content inside a 16:9 raster', () => {
  const rect = {x: 80, y: 0, width: 480, height: 360};
  candidate(paint(image(640, 360, [245, 245, 245, 255]), rect), rect);
});

test('title bar and letterboxes can have different colors', () => {
  const rect = {x: 2, y: 42, width: 640, height: 360};
  const img = image(644, 424, [245, 245, 245, 255]);
  paint(img, {x: 2, y: 22, width: 640, height: 400}, [0, 0, 0, 255]);
  paint(img, rect);
  candidate(img, rect, '16:9');
});

test('transparent border ignores hidden RGB and does not need a black pixel threshold', () => {
  const {img, rect} = windowImage(640, 360, {top: 12});
  for (let y = 0; y < img.height; y++) for (let x = 0; x < img.width; x++) {
    if (x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height) {
      img.data.set([(x * 71) % 256, (y * 43) % 256, (x + y) % 256, 0], (y * img.width + x) * 4);
    }
  }
  candidate(img, rect);
});

for (const fill of [[0, 0, 0, 255], [245, 245, 245, 255], [255, 0, 0, 255], [123, 50, 220, 0]]) {
  test(`uniform/loading image does not infer content just because it is 16:9: ${fill}`, () => {
    assert.equal(detectContent(image(640, 360, fill)).kind, 'none');
  });
}

test('ratio choice never manufactures a centered 4:3 or 16:10 crop in borderless 16:9 game pixels', () => {
  const img = paint(image(640, 360), {x: 0, y: 0, width: 640, height: 360});
  assert.equal(detectContent(img, '4:3').kind, 'none');
  assert.equal(detectContent(img, '16:10').kind, 'none');
});

test('an interior UI panel cannot be reached by skipping textured outer game pixels', () => {
  const rect = {x: 0, y: 0, width: 640, height: 360};
  const img = paint(image(640, 360), rect);
  paint(img, {x: 80, y: 25, width: 480, height: 310}, [240, 240, 240, 255]);
  paint(img, {x: 90, y: 45, width: 460, height: 270});
  candidate(img, rect);
  assert.equal(detectContent(img, '4:3').kind, 'none');
});

test('unsupported borderless aspect ratio remains unclassified', () => {
  assert.equal(detectContent(paint(image(700, 300), {x: 0, y: 0, width: 700, height: 300})).kind, 'none');
});

test('small integer rounding error is accepted, but the detected rectangle is not reshaped', () => {
  const {img, rect} = windowImage(641, 360);
  candidate(img, rect, '16:9');
});

test('the scan does not mutate its source pixel buffer', () => {
  const {img} = windowImage(640, 360);
  const before = img.data.slice();
  detectContent(img);
  assert.deepEqual(img.data, before);
});

test('a downsampled preview maps half-open edges to original-frame coordinates and labels the estimate', () => {
  const {img, rect} = windowImage(640, 360);
  const result = detectContent(img, '16:9', img.width * 3, img.height * 3);
  assert.equal(result.kind, 'candidate');
  assert.deepEqual(result.proposal.rect, {x: rect.x * 3, y: rect.y * 3, width: rect.width * 3, height: rect.height * 3});
  assert.equal(result.proposal.approximate, true);
});

test('mapping uses independent image dimensions, not CSS Fit, Retina scale, or a second DPR multiplier', () => {
  assert.deepEqual(contentToFrame({x: 1, y: 2, width: 10, height: 5}, 30, 20, 101, 67), {x: 3, y: 6, width: 35, height: 18});
  assert.deepEqual(contentToFrame({x: 0, y: 0, width: 30, height: 20}, 30, 20, 101, 67), {x: 0, y: 0, width: 101, height: 67});
});

test('malformed dimensions, pixel lengths, ratios and out-of-policy originals are refused', () => {
  const valid = image(64, 36);
  for (const bad of [{...valid, width: 0}, {...valid, height: NaN}, {...valid, width: 1.2}, {...valid, width: 16_385},
    {...valid, width: 4096, height: 2048}, {...valid, data: new Uint8ClampedArray(4)}, {...valid, data: []}]) {
    assert.throws(() => detectContent(bad), /Invalid/);
  }
  assert.throws(() => detectContent(valid, 'unknown'), /Invalid/);
  assert.throws(() => detectContent(valid, 'auto', 63, 36), /Invalid/);
  assert.throws(() => detectContent(valid, 'auto', 16_384, 16_384), /Invalid/);
  assert.throws(() => contentToFrame({x: -1, y: 0, width: 2, height: 2}, 64, 36, 64, 36), /Invalid/);
  assert.throws(() => contentToFrame({x: 63, y: 0, width: 2, height: 2}, 64, 36, 64, 36), /Invalid/);
  assert.equal(CONTENT_MAX_PIXELS, 4_194_304);
});

test('tiny rasters have insufficient evidence', () => {
  assert.equal(detectContent(image(16, 9)).kind, 'none');
});

test('display readback refuses missing, unfinished, stale or oversized images before allocating a canvas', () => {
  assert.throws(() => detectDisplayedContent(null, 'blob:current', 'auto', 640, 360), /not ready/);
  assert.throws(() => detectDisplayedContent({complete: false}, 'blob:current', 'auto', 640, 360), /not ready/);
  assert.throws(() => detectDisplayedContent({complete: true, currentSrc: 'blob:old'}, 'blob:current', 'auto', 640, 360), /not ready/);
  assert.throws(() => detectDisplayedContent({complete: true, currentSrc: 'blob:current', naturalWidth: 4096, naturalHeight: 2048},
    'blob:current', 'auto', 4096, 2048), /limit/);
});

test('display readback releases the canvas on success and on an exception', () => {
  const previous = globalThis.document;
  const {img, rect} = windowImage(640, 360);
  const element = {complete: true, currentSrc: 'blob:current', naturalWidth: img.width, naturalHeight: img.height};
  const canvases = [];
  let fail = false;
  globalThis.document = {createElement() {
    const canvas = {width: 0, height: 0, getContext() {return {
      drawImage(value) {assert.equal(value, element); if (fail) throw new Error('readback failed');},
      getImageData() {return img;},
    };}};
    canvases.push(canvas);
    return canvas;
  }};
  try {
    assert.deepEqual(detectDisplayedContent(element, 'blob:current', 'auto', img.width, img.height).proposal.rect, rect);
    fail = true;
    assert.throws(() => detectDisplayedContent(element, 'blob:current', 'auto', img.width, img.height), /readback failed/);
    assert.ok(canvases.every(canvas => canvas.width === 0 && canvas.height === 0));
  } finally { globalThis.document = previous; }
});

test('competing aspect/boundary hypotheses remain ambiguous until the ratio is explicitly restricted', () => {
  const {img, rect} = windowImage(640, 360, {left: 0, top: 40, right: 0, bottom: 2, shade: 28});
  assert.equal(detectContent(img).kind, 'ambiguous');
  candidate(img, rect, '16:9');
});

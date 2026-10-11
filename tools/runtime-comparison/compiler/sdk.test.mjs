// SDK discovery contract: node --test tools/runtime-comparison/compiler/sdk.test.mjs
// Examples are compiled and run on the controlled host by tests/sdk_examples.rs.
import assert from "node:assert/strict";
import test from "node:test";
import { methods, sdk, SDK_SEARCH_LIMITS, searchSdk, sdkDetail } from "./sdk.mjs";

const STAGES = new Set(["module", "readiness", "workflow", "closed"]);
const LANES = new Set(["controlled", "replay", "native", "desktop-native"]);
const AUTHORITIES = new Set(["none", "capture", "input", "target"]);

function ok(response) {
  assert.equal(response.ok, true, JSON.stringify(response.error));
  return response.result;
}

function refused(response) {
  assert.equal(response.ok, false);
  assert.equal(typeof response.error.message, "string");
  return response.error.code;
}

function pages(query, limit = SDK_SEARCH_LIMITS.maxLimit) {
  const collected = [];
  let cursor;
  do {
    const page = ok(searchSdk(query, cursor === undefined ? { limit } : { cursor, limit }));
    collected.push(page);
    cursor = page.next ?? undefined;
    assert.ok(collected.length <= 64, "continuation must terminate");
  } while (cursor !== undefined);
  return collected;
}

const names = collected => collected.flatMap(page => page.matches.map(match => match.name));

test("a blank query pages through every method exactly once with stateless cursors", () => {
  const listed = pages("", 5);
  assert.ok(listed.length > 1, "the fixture page size must force continuation");
  assert.deepEqual(names(listed), Object.keys(methods));
  listed.forEach((page, index) => {
    assert.equal(page.total, sdk.methods);
    assert.equal(page.offset, index * 5);
    assert.ok(page.matches.length > 0 && page.matches.length <= 5);
    assert.equal(page.complete, index === listed.length - 1);
    assert.equal(page.complete, page.next === null);
    assert.equal(page.noMatch, false);
    assert.ok(page.matches.every(match => match.match === "all"));
  });
  assert.deepEqual(pages("   ", 5).map(page => page.matches), listed.map(page => page.matches));
  const cursor = listed[0].next;
  assert.deepEqual(ok(searchSdk("", { cursor, limit: 5 })), ok(searchSdk("", { cursor, limit: 5 })));
  const wider = ok(searchSdk("", { cursor, limit: SDK_SEARCH_LIMITS.maxLimit }));
  assert.equal(wider.offset, 5);
  assert.deepEqual(names([wider]), Object.keys(methods).slice(5));
  assert.equal(wider.complete, true);
});

test("the default limit bounds a page and still reports continuation", () => {
  const page = ok(searchSdk(""));
  assert.equal(page.matches.length, Math.min(SDK_SEARCH_LIMITS.defaultLimit, page.total));
  assert.equal(page.complete, page.total <= SDK_SEARCH_LIMITS.defaultLimit);
  assert.equal(page.next === null, page.complete);
  assert.equal(ok(searchSdk("", { limit: undefined })).matches.length, page.matches.length);
});

test("invalid options are refused instead of clamped", () => {
  for (const options of [{ limit: 0 }, { limit: SDK_SEARCH_LIMITS.maxLimit + 1 }, { limit: 1.5 }, { limit: "5" },
    { limit: Number.NaN }, { offset: 1 }, null, [], "limit"]) {
    assert.equal(refused(searchSdk("ocr", options)), "sdk_options_invalid", JSON.stringify(options));
  }
});

test("cursors continue only their own normalized query in the installed SDK revision", () => {
  const first = ok(searchSdk("ocr", { limit: 1 }));
  assert.ok(first.total > 1 && first.next !== null);
  const second = ok(searchSdk("ocr", { cursor: first.next, limit: 1 }));
  assert.equal(second.offset, 1);
  assert.deepEqual(ok(searchSdk("ＯＣＲ", { cursor: first.next, limit: 1 })).matches, second.matches);
  const otherRevision = (sdk.revision[0] === "0" ? "1" : "0") + sdk.revision.slice(1);
  for (const [query, cursor] of [
    ["read text", first.next],
    ["teleport", first.next],
    ["ocr", "garbage"],
    ["ocr", first.next.replace(sdk.revision, otherRevision)],
    ["ocr", first.next.replace(/\.\d+$/, ".0")],
    ["ocr", first.next.replace(/\.\d+$/, ".01")],
    ["ocr", first.next.replace(/\.\d+$/, `.${first.total}`)],
    ["ocr", `${first.next} `],
    ["ocr", 1],
  ]) {
    assert.equal(refused(searchSdk(query, { cursor })), "sdk_cursor_invalid", `${query} ${cursor}`);
  }
});

test("unsupported purposes return an explicit empty result for this SDK", () => {
  for (const query of ["teleport the player", "瞬間移動", "???", "xyz123"]) {
    const result = ok(searchSdk(query));
    assert.deepEqual([result.total, result.matches, result.noMatch, result.next, result.complete], [0, [], true, null, true], query);
    assert.deepEqual(result.sdk, { ...sdk });
  }
});

test("an exact method name ranks first regardless of case and separators", () => {
  for (const name of Object.keys(methods)) {
    for (const query of [name, name.toUpperCase(), name.replaceAll("_", " ")]) {
      const [first] = ok(searchSdk(query)).matches;
      assert.deepEqual([first.name, first.match], [name, "exact"], query);
    }
  }
});

test("documented English and Japanese purposes find the supported operation", () => {
  for (const [query, expected] of [
    ["read multiple OCR regions", "scan_ocr_zones"],
    ["複数のOCR領域を読み取りたい", "scan_ocr_zones"],
    ["take a screenshot", "observe"],
    ["マウスでクリック", "submit"],
    ["一時停止", "wait"],
  ]) {
    const [first] = ok(searchSdk(query)).matches;
    assert.equal(first.name, expected, query);
    assert.notEqual(first.match, "exact");
    assert.ok(first.terms.length > 0);
  }
  for (const [name, [, , , guidance]] of Object.entries(methods)) {
    assert.ok(guidance.purpose.en.length > 0 && guidance.purpose.ja.length > 0, name);
    for (const term of [...guidance.purpose.en, ...guidance.purpose.ja]) {
      assert.ok(names(pages(term)).includes(name), `${term} must find ${name}`);
    }
  }
});

test("detail returns the full contract and examples for every listed method", () => {
  const sources = new Map();
  for (const name of names(pages(""))) {
    const result = ok(sdkDetail(name));
    assert.deepEqual([result.name, result.noMatch, result.sdk], [name, false, { ...sdk }]);
    assert.ok(result.rules.length > 0);
    const { method } = result;
    assert.equal(method.name, name);
    for (const field of ["args", "result", "description"]) assert.ok(typeof method[field] === "string" && method[field] !== "");
    assert.ok(method.constraints.length > 0 && method.examples.length > 0);
    assert.equal(method.availability.declared, true);
    assert.ok(method.availability.stages.length > 0 && method.availability.stages.every(stage => STAGES.has(stage)));
    assert.ok(method.availability.lanes.length > 0 && method.availability.lanes.every(lane => LANES.has(lane)));
    assert.ok(AUTHORITIES.has(method.availability.authority));
    for (const example of method.examples) {
      assert.ok(example.title !== "" && example.source !== "", example.id);
      assert.equal(sources.get(example.id) ?? example.source, example.source, example.id);
      sources.set(example.id, example.source);
    }
  }
});

test("unknown or prototype names are explicit no-match rather than guesses", () => {
  for (const name of ["teleport", "Observe", "observe ", "constructor", "__proto__", "toString", "hasOwnProperty", "host.call"]) {
    const result = ok(sdkDetail(name));
    assert.deepEqual([result.name, result.noMatch, result.method], [name, true, null], name);
    assert.deepEqual(result.sdk, { ...sdk });
  }
  for (const name of ["", "x".repeat(SDK_SEARCH_LIMITS.queryUnits + 1), 42, null, undefined, {}]) {
    assert.equal(refused(sdkDetail(name)), "sdk_name_invalid");
  }
});

test("queries outside the advertised bound are refused", () => {
  assert.equal(ok(searchSdk("x".repeat(SDK_SEARCH_LIMITS.queryUnits))).noMatch, true);
  for (const query of ["x".repeat(SDK_SEARCH_LIMITS.queryUnits + 1), null, 1, undefined]) {
    assert.equal(refused(searchSdk(query)), "sdk_query_invalid");
  }
});

test("responses are JSON-safe and within the advertised byte bound", () => {
  const results = [...pages(""), ...pages("ocr"), ...pages("待機"), ...Object.keys(methods).map(name => ok(sdkDetail(name)))];
  for (const result of results) {
    const text = JSON.stringify(result);
    assert.deepEqual(JSON.parse(text), result);
    assert.ok(Buffer.byteLength(text) <= SDK_SEARCH_LIMITS.responseBytes, `${Buffer.byteLength(text)} bytes`);
  }
});

test("callers cannot alter the authority through returned data", () => {
  const before = ok(sdkDetail("observe"));
  const mutated = ok(sdkDetail("observe"));
  mutated.method.constraints.push("changed");
  mutated.method.purpose.en.length = 0;
  mutated.method.examples[0].source = "";
  mutated.sdk.revision = "changed";
  ok(searchSdk("observe")).matches[0].terms.push("changed");
  assert.deepEqual(ok(sdkDetail("observe")), before);
  assert.deepEqual(ok(searchSdk("observe")).matches[0].terms, ["observe"]);
  assert.ok(Object.isFrozen(sdk) && Object.isFrozen(methods.observe) && Object.isFrozen(methods.observe[3].availability.stages));
});

test("the SDK identity is deterministic for the installed catalog", async () => {
  assert.equal(sdk.contract, "mado-host-v1");
  assert.match(sdk.revision, /^[0-9a-f]{16}$/);
  assert.equal(sdk.methods, Object.keys(methods).length);
  const reloaded = await import(`./sdk.mjs?reload=${Date.now()}`);
  assert.notEqual(reloaded.methods, methods);
  assert.deepEqual(reloaded.sdk, sdk);
});

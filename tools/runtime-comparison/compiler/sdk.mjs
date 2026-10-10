// mado-host-v1: JSON values only; native resources remain owned by Rust.
// The one SDK authority: completion declarations and discovery both read `methods`.
// Entries are [argument type, result type, description, guidance]. Guidance holds
// English/Japanese purpose terms, behavioral constraints, availability and example IDs.
// Availability is static host admission (stage, lane, native authority), not readiness.
export const methods = deepFreeze({
  target_start: ["Record<string, never>", "MadoNativeProgress",
    "Request target preparation once during Desktop Native Readiness using the saved binding and reviewed authority. Returns pending without waiting for a window; unavailable in other stages and lanes.",
    {
      purpose: {
        en: ["start target", "launch game", "start game", "target startup", "native readiness"],
        ja: ["ターゲット起動", "ゲーム起動", "ゲームを起動", "起動要求", "ネイティブ準備"],
      },
      constraints: [
        "Arguments must be exactly {}; no path, target, recipe, process or authority override is accepted.",
        "Only Desktop Native Readiness with a saved target binding and reviewed authority admits it; other lanes and workflow refuse with Authority, module evaluation with AdmissionClosed.",
        "Single use: a second request refuses with NativeStartRefused.",
        "Returns pending without waiting for a window; poll target_status with finite waits.",
      ],
      availability: { stages: ["readiness"], lanes: ["desktop-native"], authority: "target" },
      examples: ["native-target-startup"],
    }],
  target_status: ["Record<string, never>", "MadoNativeProgress",
    "Poll target preparation during Desktop Native Readiness. Capture availability, preparation phase and launch disposition are separate; capture_ready does not establish game readiness.",
    {
      purpose: {
        en: ["target status", "poll target", "capture ready", "launch status", "startup progress"],
        ja: ["ターゲット状態", "起動状態", "キャプチャ準備", "起動の進捗"],
      },
      constraints: [
        "Arguments must be exactly {}; only Desktop Native Readiness admits it. Other lanes and workflow refuse with Authority, module evaluation with AdmissionClosed.",
        "status, phase and launch are independent: capture_ready makes capture available but does not mean the game is ready.",
        "pending is progress, not an error; bound polling with wait and a probe count, and check visible criteria before returning Ready.",
      ],
      availability: { stages: ["readiness"], lanes: ["desktop-native"], authority: "target" },
      examples: ["native-target-startup"],
    }],
  observe: ["Record<string, never>", "MadoObservation",
    "Retain an eligible capture-pixel observation with run, process lifetime, session, geometry and frame identity. Release its id when finished.",
    {
      purpose: {
        en: ["capture frame", "current frame", "screenshot", "take screenshot", "observation", "grab screen"],
        ja: ["画面取得", "画面を取得", "キャプチャ", "スクリーンショット", "観測", "現在のフレーム"],
      },
      constraints: [
        "Arguments must be exactly {}.",
        "Each call retains a new observation handle charged to the attempt's handle budget; release its id on every exit path.",
        "Unavailable during module evaluation; in Desktop Native Readiness it refuses with TargetNotReady until target_status reports capture_ready.",
        "Use the frame identity, not arrival time, when freshness matters.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["observation-lifetime"],
    }],
  asset: ["{ id: string }", "{ readonly id: string; readonly bytes: number }",
    "Look up an asset in the captured package inventory and return its id and byte size, without exposing native resources.",
    {
      purpose: {
        en: ["package asset", "asset lookup", "template asset", "template image", "asset size"],
        ja: ["アセット", "パッケージのアセット", "テンプレート画像", "素材"],
      },
      constraints: [
        "id must name an asset declared in the captured package inventory; an unknown id fails with MissingAsset.",
        "Returns only the id and byte size: no pixels and no handle to release.",
        "Also available during module evaluation.",
      ],
      availability: { stages: ["module", "readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "none" },
      examples: ["template-match"],
    }],
  recognize: ["MadoRecognitionRequest", "MadoRecognition | null",
    "Recognize a template or OCR region in a retained observation. Returns null when no match is found; release a returned recognition id when finished.",
    {
      purpose: {
        en: ["recognize", "find template", "template match", "image match", "read text", "ocr", "single region"],
        ja: ["認識", "テンプレート照合", "テンプレートマッチ", "画像認識", "画像を探す", "文字認識", "文字を読む"],
      },
      constraints: [
        "kind \"template\" requires a captured template asset id; kind \"ocr\" must not pass asset.",
        "roi is in capture pixels and must lie inside the retained observation's extent.",
        "Returns null when nothing matches; a non-null result holds a handle to release.",
        "A backend failure is a typed fault, never null.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["template-match", "bounded-poll"],
    }],
  scan_ocr_zones: ["MadoOcrZoneScanRequest", "MadoOcrZoneScanResult",
    "Scan normalized OCR zones in one retained observation and return ordered recognized or no_match outcomes. The result has no handle to release; the caller still owns the observation.",
    {
      purpose: {
        en: ["grouped ocr", "multiple ocr regions", "read multiple regions", "several regions", "batch ocr", "ocr zones", "read text", "ocr"],
        ja: ["複数のOCR", "複数領域", "一括OCR", "まとめてOCR", "OCR領域", "文字を読む", "文字認識"],
      },
      constraints: [
        "One request over one retained observation; basis frame_width/frame_height must equal the observation's size and every normalized region must lie within basis.content.",
        "Zone ids must be distinct, non-empty, free of control characters and at most 256 bytes; send 1 to the engine's grouped-zone limit (8 at the current native pin). Larger selections are refused, never split.",
        "Zones return in request order as recognized or no_match with every returned region; no_match is explicit absence, not empty text.",
        "The result has no handle; the caller still releases the observation.",
        "More than 256 regions or 256 KiB of results is a fault, never truncated success.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["grouped-ocr"],
    }],
  query: ["MadoRecognitionRequest & { expected?: string }", "{ readonly id: string }",
    "Create a managed template or OCR query from a retained observation. OCR may require expected text; use query_wait with a finite timeout and release the query id when finished.",
    {
      purpose: {
        en: ["wait for text", "wait for image", "wait until visible", "visual query", "expected text", "managed query"],
        ja: ["表示を待つ", "文字を待つ", "画像を待つ", "出現待ち", "クエリ"],
      },
      constraints: [
        "Creates a managed template or OCR query over a retained observation; OCR may set non-empty expected text within the run's text bound, template queries must not.",
        "Returns only an id: wait for it with query_wait and a finite timeout.",
        "Release the query when finished; a successful result can retain a separate recognition and observation.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["visual-query"],
    }],
  query_wait: ["{ id: string; timeout_ms: number }", "MadoRecognition",
    "Wait within a finite timeout for a managed visual query to match. Exhaustion is a typed timeout, not a null result.",
    {
      purpose: {
        en: ["wait for match", "wait until visible", "finite wait", "timeout", "poll until"],
        ja: ["一致を待つ", "表示を待つ", "タイムアウト", "待機"],
      },
      constraints: [
        "timeout_ms must be positive and within the run's wait limit.",
        "Exhaustion fails with category Timeout, which ends the attempt; it never returns null or an unmatched frame as success.",
        "Release the returned recognition and observation, the query, and the original observation when finished. IDs can alias on the controlled host; release each distinct ID only once.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["visual-query"],
    }],
  submit: ["{ observation: MadoObservation; actions: readonly MadoAction[] }", "{ readonly id: string; readonly order: number }",
    "Admit a bounded, ordered input sequence against its retained observation and return a sequence id. Admission is not application-effect proof; settle the sequence and independently check the expected effect.",
    {
      purpose: {
        en: ["press key", "key press", "click", "send input", "keyboard", "mouse", "input sequence"],
        ja: ["キー入力", "キーを押す", "クリック", "入力送信", "キーボード", "マウス", "入力"],
      },
      constraints: [
        "Workflow only; module evaluation, readiness and closed admission refuse it.",
        "Send 1 to the run's action limit of key_down/key_up actions with portable key names (ASCII letters, digits or _, at most 32) or click actions inside the observation's capture-pixel extent.",
        "The observation must still be retained and current; route, focus and queue capacity are checked at admission.",
        "Controlled and replay runs use a non-native sink; native runs need separately reviewed input authority.",
        "Admission is not effect proof: settle the sequence and check an independent postcondition.",
      ],
      availability: { stages: ["workflow"], lanes: ["controlled", "replay", "native"], authority: "input" },
      examples: ["input-receipt"],
    }],
  settle: ["{ id: string }", "MadoReceipt",
    "Settle an admitted input sequence and return its submission and cleanup receipt. Submitted, Partial and Uncertain do not prove application effect; uncertain input is not automatically replayed.",
    {
      purpose: {
        en: ["input receipt", "settle input", "input result", "dispatch result"],
        ja: ["入力結果", "入力の完了", "入力の確定", "受領"],
      },
      constraints: [
        "Returns receipt status Submitted, Partial, Uncertain, Refused or Cancelled; none of them proves the application effect.",
        "Partial or uncertain input is never replayed automatically; report it instead of retrying.",
        "Release the sequence id after reading the receipt. Settling queued input after admission closes reports a refusal instead of dispatching.",
      ],
      availability: { stages: ["workflow", "closed"], lanes: ["controlled", "replay", "native"], authority: "input" },
      examples: ["input-receipt"],
    }],
  postcondition: ["{ observation: MadoObservation; checkpoint: MadoObservation; expected: string }", "{ readonly satisfied: boolean; readonly frame: number; readonly checkpoint: number }",
    "Check the expected visual condition against a strictly newer compatible observation than the checkpoint. Frame freshness or input submission alone does not establish the expected effect.",
    {
      purpose: {
        en: ["verify effect", "check result", "confirm screen changed", "expected screen", "postcondition"],
        ja: ["効果の確認", "結果確認", "画面の変化を確認", "事後条件"],
      },
      constraints: [
        "observation must be strictly newer than checkpoint in the same session and geometry; otherwise it fails with StaleFrame.",
        "satisfied reports whether the expected visual condition is present in observation; input submission or a newer frame alone is not the effect.",
        "Both observations stay owned by the caller.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "capture" },
      examples: ["input-receipt"],
    }],
  release: ["{ id: string }", "{ readonly released: true }",
    "Release an attempt-owned managed handle or queued sequence. Remains available after ordinary admission closes; releasing a handle does not prove physical cleanup or application effect.",
    {
      purpose: {
        en: ["release handle", "free observation", "cleanup", "dispose", "release"],
        ja: ["解放", "ハンドル解放", "後始末", "クリーンアップ"],
      },
      constraints: [
        "Releases an observation, recognition, query or sequence id owned by this attempt; unknown or already released ids fail with InvalidHandle.",
        "Remains available after ordinary admission closes; releasing a queued sequence cancels it.",
        "Release neither undoes input nor proves physical cleanup or application effect.",
      ],
      availability: { stages: ["readiness", "workflow", "closed"], lanes: ["controlled", "replay", "native"], authority: "none" },
      examples: ["observation-lifetime"],
    }],
  wait: ["{ duration_ms: number }", "{ readonly elapsed_ms: number }",
    "Wait for a positive, bounded duration while preserving cancellation and admission checks, then report elapsed milliseconds.",
    {
      purpose: {
        en: ["sleep", "delay", "pause", "wait", "wait milliseconds"],
        ja: ["待機", "待つ", "一時停止", "遅延", "スリープ"],
      },
      constraints: [
        "duration_ms must be positive and within the run's wait limit.",
        "Stop, cancellation and stage deadlines interrupt the wait; unavailable during module evaluation.",
        "Bound polling loops with a probe count as well as waits.",
      ],
      availability: { stages: ["readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "none" },
      examples: ["bounded-poll"],
    }],
  log: ["{ message: string }", "{ readonly recorded: boolean }",
    "Record a bounded Script message. Returns recorded: false when the log budget is exhausted; logging does not grant host authority.",
    {
      purpose: {
        en: ["log message", "log", "print", "debug output", "record message"],
        ja: ["ログ", "ログ出力", "メッセージ記録", "デバッグ出力"],
      },
      constraints: [
        "message is kept within the run's record and byte budget; recorded: false means it was dropped, not an error.",
        "Logs are diagnostics only and grant no host authority; do not log private recognized text by default.",
        "Also available during module evaluation.",
      ],
      availability: { stages: ["module", "readiness", "workflow"], lanes: ["controlled", "replay", "native"], authority: "none" },
      examples: ["observation-lifetime"],
    }],
});

const READY_TEXT_SOURCE = `function readyTextVisible(): boolean {
  const observation = host.call("observe", {});
  try {
    const text = host.call("recognize", {
      observation,
      kind: "ocr",
      roi: { x: 0, y: 0, width: observation.width, height: observation.height },
    });
    if (text === null) return false;
    try {
      return text.text === "READY";
    } finally {
      host.call("release", { id: text.id });
    }
  } finally {
    host.call("release", { id: observation.id });
  }
}`;

// Complete package modules (main.ts with both entries) that compile with the pinned
// compiler under any options schema. Reading them never runs them.
const examples = deepFreeze({
  "observation-lifetime": {
    title: "Retain and release an observation",
    source: `// Retain one observation, use it and release it on every exit path.
export function readiness(): MadoReady {
  return "Ready";
}

export function workflow(): void {
  const observation = host.call("observe", {});
  try {
    if (observation.width < 1 || observation.height < 1) {
      throw new Error("Capture extent is empty");
    }
    host.call("log", { message: "frame " + observation.frame + ": " + observation.width + "x" + observation.height });
  } finally {
    host.call("release", { id: observation.id });
  }
}
`,
  },
  "grouped-ocr": {
    title: "Read several OCR regions in one request",
    source: `// Read several OCR regions from one observation in a single request.
// Replace recognitionBasis with the Game content setup copied from Recognition.
const recognitionBasis: MadoRecognitionBasis = {
  frame_width: 640,
  frame_height: 480,
  content: { x: 0, y: 0, width: 640, height: 480 },
};

export function readiness(): MadoReady {
  return "Ready";
}

export function workflow(): void {
  const observation = host.call("observe", {});
  try {
    if (observation.width !== recognitionBasis.frame_width || observation.height !== recognitionBasis.frame_height) {
      throw new Error("Recognition geometry basis changed");
    }
    const scan = host.call("scan_ocr_zones", {
      observation,
      basis: recognitionBasis,
      zones: [
        { id: "title", region: { u0: 0.1, v0: 0.1, u1: 0.4, v1: 0.3 } },
        { id: "status", region: { u0: 0.6, v0: 0.8, u1: 0.95, v1: 0.95 } },
      ],
    });
    // Zones keep request order; no_match is explicit absence, not empty text.
    for (const zone of scan.zones) {
      host.call("log", { message: zone.id + ": " + (zone.outcome === "recognized" ? zone.regions.length + " region(s)" : "no_match") });
    }
  } finally {
    // The scan has no handle; the observation still belongs to this Script.
    host.call("release", { id: observation.id });
  }
}
`,
  },
  "template-match": {
    title: "Find a package template",
    source: `// Look for a package template in the whole frame; null means not found.
export function readiness(): MadoReady {
  return "Ready";
}

export function workflow(): void {
  // Fails with MissingAsset when the package does not declare "marker".
  const marker = host.call("asset", { id: "marker" });
  const observation = host.call("observe", {});
  try {
    const match = host.call("recognize", {
      observation,
      kind: "template",
      asset: marker.id,
      roi: { x: 0, y: 0, width: observation.width, height: observation.height },
    });
    if (match === null) {
      host.call("log", { message: "marker: no_match" });
      return;
    }
    try {
      host.call("log", { message: "marker: score " + match.score.toFixed(2) });
    } finally {
      host.call("release", { id: match.id });
    }
  } finally {
    host.call("release", { id: observation.id });
  }
}
`,
  },
  "visual-query": {
    title: "Wait a finite time for visible text",
    source: `// Wait at most 500 ms for OCR text, then release every distinct returned handle.
export function readiness(): MadoReady {
  return "Ready";
}

export function workflow(): void {
  const observation = host.call("observe", {});
  try {
    const query = host.call("query", {
      observation,
      kind: "ocr",
      roi: { x: 0, y: 0, width: observation.width, height: observation.height },
      expected: "READY",
    });
    try {
      // Exhaustion ends the attempt with a typed Timeout, never null or a stale success.
      const match = host.call("query_wait", { id: query.id, timeout_ms: 500 });
      try {
        host.call("log", { message: "ready: frame " + match.observation.frame });
      } finally {
        if (match.id !== query.id) host.call("release", { id: match.id });
        if (match.observation.id !== query.id && match.observation.id !== observation.id) {
          host.call("release", { id: match.observation.id });
        }
      }
    } finally {
      host.call("release", { id: query.id });
    }
  } finally {
    host.call("release", { id: observation.id });
  }
}
`,
  },
  "input-receipt": {
    title: "Submit input and check its effect",
    source: `// Press one key, settle the receipt, then check a strictly newer frame.
// Receipts never prove the effect, and partial or uncertain input is not replayed.
export function readiness(): MadoReady {
  return "Ready";
}

export function workflow(): void {
  const before = host.call("observe", {});
  try {
    const sequence = host.call("submit", {
      observation: before,
      actions: [{ kind: "key_down", key: "A" }, { kind: "key_up", key: "A" }],
    });
    const receipt = settleOnce(sequence.id);
    if (receipt.status !== "Submitted") {
      host.call("log", { message: "input: " + receipt.status });
      return;
    }
    const after = host.call("observe", {});
    try {
      const check = host.call("postcondition", { observation: after, checkpoint: before, expected: "DONE" });
      host.call("log", { message: check.satisfied ? "effect: confirmed" : "effect: absent" });
    } finally {
      host.call("release", { id: after.id });
    }
  } finally {
    host.call("release", { id: before.id });
  }
}

function settleOnce(id: string): MadoReceipt {
  try {
    return host.call("settle", { id });
  } finally {
    // Frees the sequence handle; it does not undo input.
    host.call("release", { id });
  }
}
`,
  },
  "bounded-poll": {
    title: "Poll a bounded number of frames during readiness",
    source: `// Poll at most five frames for OCR text before declaring readiness.
// Give up with an explicit error instead of looping without a bound.
const PROBES = 5;

export function readiness(): MadoReady {
  for (let probe = 1; ; probe++) {
    if (readyTextVisible()) return "Ready";
    if (probe === PROBES) throw new Error("Ready text was not recognized in " + PROBES + " frames");
    host.call("wait", { duration_ms: 100 });
  }
}

export function workflow(): void {}

${READY_TEXT_SOURCE}
`,
  },
  "native-target-startup": {
    title: "Start and poll the Desktop Native target",
    source: `// Desktop Native Readiness only: request startup once, poll with finite waits,
// then require the expected "READY" text. capture_ready alone is not game readiness.
export function readiness(): MadoReady {
  let target = host.call("target_start", {});
  for (let probe = 0; probe < 120 && target.status !== "capture_ready"; probe++) {
    host.call("wait", { duration_ms: 500 });
    target = host.call("target_status", {});
  }
  if (target.status !== "capture_ready") throw new Error("CaptureNotReady");
  if (!readyTextVisible()) throw new Error("Expected READY text is not visible");
  return "Ready";
}

export function workflow(): void {}

${READY_TEXT_SOURCE}
`,
  },
});

function deepFreeze(value) {
  if (value !== null && typeof value === "object") {
    for (const item of Object.values(value)) deepFreeze(item);
    Object.freeze(value);
  }
  return value;
}

function optionType(node, depth = 0) {
  if (!node || depth > 32) throw new Error("Invalid or excessively nested option schema");
  if (Array.isArray(node.enum)) return node.enum.map(value => JSON.stringify(value)).join(" | ");
  switch (node.type) {
    case "object": {
      const required = new Set(node.required ?? []);
      return `{ ${Object.entries(node.properties).map(([name, value]) =>
        `readonly ${JSON.stringify(name)}${required.has(name) || (depth === 0 && Object.hasOwn(value, "default")) ? "" : "?"}: ${optionType(value, depth + 1)};`).join(" ")} }`;
    }
    case "array": return `ReadonlyArray<${optionType(node.items, depth + 1)}>`;
    case "integer":
    case "number": return "number";
    case "string": return "string";
    case "boolean": return "boolean";
    default: throw new Error(`Unsupported schema type: ${node.type}`);
  }
}

export function definitions(schema) {
  return declarations(optionType(schema));
}

// Advisory editing only; invalid schemas remain errors in definitions().
export function unknownOptionsDefinitions() {
  return declarations("unknown");
}

function declarations(options) {
  return `// Generated from the captured schema and application-owned mado-host-v1 contract.
type MadoOptions = ${options};
// Desktop Native Readiness only. Exact empty payloads; no target or launch overrides.
interface MadoNativeProgress {
  readonly attempt: number;
  readonly status: "not_requested" | "pending" | "capture_ready";
  readonly phase: "preflight" | "target_discovery" | "launch_submission" | "waiting_for_process" | "waiting_for_window" | "native_initialization" | "readiness" | "workflow" | "settling" | "recovering";
  readonly launch: "not_requested" | "accepted" | "rejected" | "uncertain";
}
interface MadoRegion { readonly x: number; readonly y: number; readonly width: number; readonly height: number }
interface MadoObservation {
  readonly id: string; readonly run: string; readonly attempt: number;
  readonly process_lifetime: string; readonly session: number | string; readonly geometry: number;
  readonly epoch?: number | string;
  readonly frame: number; readonly width: number; readonly height: number;
  readonly coordinate_space: "capture-pixels";
}
type MadoRecognitionRequest = { observation: MadoObservation; roi: MadoRegion } &
  ({ kind: "template"; asset: string } | { kind: "ocr"; asset?: never });
interface MadoRecognition {
  readonly id: string; readonly observation: MadoObservation;
  readonly kind: "template" | "ocr"; readonly region: MadoRegion;
  readonly score: number; readonly text?: string;
}
interface MadoRecognitionBasis {
  readonly frame_width: number; readonly frame_height: number; readonly content: MadoRegion;
}
interface MadoNormalizedRegion {
  readonly u0: number; readonly v0: number; readonly u1: number; readonly v1: number;
}
interface MadoOcrZoneScanRequest {
  readonly observation: MadoObservation;
  readonly basis: MadoRecognitionBasis;
  readonly zones: ReadonlyArray<{ readonly id: string; readonly region: MadoNormalizedRegion }>;
}
interface MadoOcrZoneScanResult {
  readonly kind: "ocr"; readonly observation: MadoObservation;
  readonly text_contract: "facade-nfc-unicode-trimmed-no-additional-application-normalization";
  readonly zones: ReadonlyArray<{
    readonly id: string; readonly outcome: "recognized" | "no_match";
    readonly regions: ReadonlyArray<{
      readonly text: string; readonly confidence: number; readonly bounds: MadoRegion;
      readonly geometry: readonly [
        readonly [number, number], readonly [number, number],
        readonly [number, number], readonly [number, number]
      ];
    }>;
  }>;
}
type MadoAction =
  | { readonly kind: "key_down" | "key_up"; readonly key: string; readonly x?: never; readonly y?: never; readonly button?: never }
  | { readonly kind: "click"; readonly x: number; readonly y: number; readonly button: "left" | "right" | "middle"; readonly key?: never };
interface MadoReceipt {
  readonly id: string; readonly order: number;
  readonly status: "Submitted" | "Partial" | "Uncertain" | "Refused" | "Cancelled";
  readonly submitted: number; readonly total: number; readonly reason?: string;
  readonly cleanup_required: readonly string[]; readonly sink: "controlled-non-native" | "native";
  readonly outcome?: string; readonly submitted_events?: number; readonly total_events?: number;
  readonly last_submitted_event?: number | null;
  readonly selected_route?: "system" | "window_message" | "process_directed" | null;
  readonly address_scope?: string | null; readonly evidence?: string | null;
  readonly used_fallback?: boolean; readonly partial_native_effect?: boolean; readonly possible_native_effect?: boolean;
  readonly cleanup?: { readonly state: string; readonly released: number; readonly owed: number; readonly may_leave_state_held: boolean };
  readonly application_effect_confirmed?: false;
}
interface MadoHostFault { readonly category: string; readonly message: string; readonly context: unknown }
type MadoReady = "Ready";
interface MadoCalls {
${Object.entries(methods).map(([method, [args, result, documentation]]) =>
  `  /** ${documentation} */\n  ${method}: { args: ${args}; result: ${result} };`).join("\n")}
}
declare const host: {
  readonly options: MadoOptions;
  readonly state: Record<string, unknown>;
  /** Invoke a typed mado-host-v1 operation with JSON arguments and managed identities. Runtime stage, authority and resource checks still apply. */
  call<K extends keyof MadoCalls>(method: K, args: MadoCalls[K]["args"]): MadoCalls[K]["result"];
};
`;
}

// Discovery: synchronous, JSON-safe search and detail over the same `methods` table.
// Results never come from completion state, external documentation or a model.
const CONTRACT = "mado-host-v1";
export const SDK_SEARCH_LIMITS = Object.freeze({
  defaultLimit: 8,
  maxLimit: 32,
  // Maximum UTF-16 units in a query or method name argument.
  queryUnits: 256,
  // Maximum JSON bytes of any successful search page or detail result.
  responseBytes: 32768,
});
const RULES = Object.freeze([
  "Search and detail never run code, discover native resources or grant authority; host stage, lane and authority checks still decide every call.",
  "desktop-native is the reviewed Desktop Native subset of native: methods listing native also apply there, subject to their declared stages, capabilities and authority.",
  "Any host fault, including Timeout, ends the attempt with that typed fault; catching it does not restore admission, and host teardown releases what the Script still holds.",
  "Release every returned handle id on normal and exceptional exits; release stays available after admission closes.",
]);

const SEPARATORS = /[\s_\-.,:;!?'"`()[\]{}<>\/\\|、。・「」『』【】〈〉《》]+/gu;
const WORD = /[a-z0-9]/;
const CURSOR = /^([0-9a-f]{16})\.([0-9a-f]{16})\.([1-9][0-9]{0,8})$/;

function normalize(text) {
  return text.normalize("NFKC").toLowerCase().replace(SEPARATORS, " ").trim();
}

function terms(texts) {
  const seen = new Map();
  for (const text of texts) {
    const key = normalize(text);
    if (key !== "" && !seen.has(key)) seen.set(key, text);
  }
  return [...seen].map(([key, text]) => ({ key, text }));
}

const catalog = Object.entries(methods).map(([name, [args, result, description, guidance]], index) => ({
  name, args, result, description, guidance, index,
  key: normalize(name),
  names: terms([name, ...name.split("_")]),
  purposes: terms([...guidance.purpose.en, ...guidance.purpose.ja]),
}));
const byName = new Map(catalog.map(entry => [entry.name, entry]));

// Change detection for the installed catalog and cursor binding; not an integrity hash.
function digest(text) {
  let first = 0x811c9dc5;
  let second = 0x9e3779b9;
  for (let index = 0; index < text.length; index++) {
    const unit = text.charCodeAt(index);
    first = Math.imul(first ^ unit, 0x01000193);
    second = Math.imul(second ^ unit, 0x5bd1e995);
    second ^= second >>> 13;
  }
  return hex(first) + hex(second);
}

function hex(value) {
  return (value >>> 0).toString(16).padStart(8, "0");
}

const IDENTITY = Object.freeze({
  contract: CONTRACT,
  revision: digest(JSON.stringify([CONTRACT, methods, examples, RULES, declarations("unknown")])),
  methods: catalog.length,
});
export const sdk = IDENTITY;

// `needle` occurs in `haystack` without splitting an ASCII word at its edges.
function occurs(haystack, needle, wholeEnd) {
  const startsWord = WORD.test(needle[0]);
  const endsWord = WORD.test(needle[needle.length - 1]);
  for (let at = haystack.indexOf(needle); at !== -1; at = haystack.indexOf(needle, at + 1)) {
    const before = at === 0 ? "" : haystack[at - 1];
    const after = haystack[at + needle.length] ?? "";
    if ((!startsWord || !WORD.test(before)) && (!wholeEnd || !endsWord || !WORD.test(after))) return true;
  }
  return false;
}

// A documented term matches when the query contains it, or a query of at least two
// characters starts a word of it ("ocr" finds "grouped ocr").
function hits(query, term) {
  return occurs(query, term, true) || ([...query].length >= 2 && occurs(term, query, false));
}

function rank(query, everything) {
  const rows = [];
  for (const entry of catalog) {
    if (everything) {
      rows.push({ entry, match: "all", terms: [], score: 0 });
      continue;
    }
    if (query === "") continue;
    if (query === entry.key) {
      rows.push({ entry, match: "exact", terms: [entry.name], score: Number.MAX_SAFE_INTEGER });
      continue;
    }
    const found = new Map();
    for (const term of entry.names) if (hits(query, term.key)) found.set(term.key, term.text);
    const named = found.size > 0;
    for (const term of entry.purposes) if (!found.has(term.key) && hits(query, term.key)) found.set(term.key, term.text);
    if (found.size > 0) rows.push({ entry, match: named ? "name" : "purpose", terms: [...found.values()], score: found.size });
  }
  // Deterministic for a catalog revision, so continuation pages are stable.
  return rows.sort((a, b) => (b.score - a.score) || ((a.match === "name" ? 0 : 1) - (b.match === "name" ? 0 : 1))
    || (a.entry.index - b.entry.index));
}

function failure(code, message) {
  return { ok: false, error: { code, message } };
}

export function searchSdk(query, options) {
  if (typeof query !== "string" || query.length > SDK_SEARCH_LIMITS.queryUnits) {
    return failure("sdk_query_invalid", `Query must be a string of at most ${SDK_SEARCH_LIMITS.queryUnits} UTF-16 units`);
  }
  let cursor;
  let limit = SDK_SEARCH_LIMITS.defaultLimit;
  if (options !== undefined) {
    if (options === null || typeof options !== "object" || Array.isArray(options)
        || Object.keys(options).some(key => key !== "cursor" && key !== "limit")) {
      return failure("sdk_options_invalid", "Options may contain only cursor and limit");
    }
    if (options.limit !== undefined) limit = options.limit;
    cursor = options.cursor;
  }
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > SDK_SEARCH_LIMITS.maxLimit) {
    return failure("sdk_options_invalid", `Limit must be an integer from 1 to ${SDK_SEARCH_LIMITS.maxLimit}`);
  }
  const key = normalize(query);
  const rows = rank(key, query.trim() === "");
  const scope = digest(key);
  let offset = 0;
  if (cursor !== undefined) {
    const parts = typeof cursor === "string" ? CURSOR.exec(cursor) : null;
    offset = parts && parts[1] === IDENTITY.revision && parts[2] === scope ? Number(parts[3]) : 0;
    if (offset === 0 || offset >= rows.length) {
      return failure("sdk_cursor_invalid", "Cursor does not continue this query in the installed SDK; search again without it");
    }
  }
  const end = Math.min(offset + limit, rows.length);
  const next = end < rows.length ? `${IDENTITY.revision}.${scope}.${end}` : null;
  return {
    ok: true,
    result: {
      sdk: { ...IDENTITY },
      query,
      total: rows.length,
      offset,
      matches: rows.slice(offset, end).map(({ entry, match, terms: matched }) => ({
        name: entry.name, args: entry.args, result: entry.result, description: entry.description,
        match, terms: matched,
      })),
      noMatch: rows.length === 0,
      next,
      complete: next === null,
    },
  };
}

export function sdkDetail(name) {
  if (typeof name !== "string" || name.length === 0 || name.length > SDK_SEARCH_LIMITS.queryUnits) {
    return failure("sdk_name_invalid", `Method name must be a non-empty string of at most ${SDK_SEARCH_LIMITS.queryUnits} UTF-16 units`);
  }
  const entry = byName.get(name);
  const base = { sdk: { ...IDENTITY }, name, rules: [...RULES] };
  if (entry === undefined) return { ok: true, result: { ...base, noMatch: true, method: null } };
  const { purpose, constraints, availability, examples: ids } = entry.guidance;
  return {
    ok: true,
    result: {
      ...base,
      noMatch: false,
      method: {
        name: entry.name, args: entry.args, result: entry.result, description: entry.description,
        purpose: { en: [...purpose.en], ja: [...purpose.ja] },
        constraints: [...constraints],
        availability: {
          declared: true,
          stages: [...availability.stages],
          lanes: [...availability.lanes],
          authority: availability.authority,
        },
        examples: ids.map(id => ({ id, title: examples[id].title, source: examples[id].source })),
      },
    },
  };
}

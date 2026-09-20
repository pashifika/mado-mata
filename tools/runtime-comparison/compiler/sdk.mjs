// mado-host-v1: JSON values only; native resources remain owned by Rust.
export const methods = Object.freeze({
  observe: ["Record<string, never>", "MadoObservation"],
  asset: ["{ id: string }", "{ readonly id: string; readonly bytes: number }"],
  recognize: ["MadoRecognitionRequest", "MadoRecognition | null"],
  query: ["MadoRecognitionRequest & { expected?: string }", "{ readonly id: string }"],
  query_wait: ["{ id: string; timeout_ms: number }", "MadoRecognition"],
  submit: ["{ observation: MadoObservation; actions: readonly MadoAction[] }", "{ readonly id: string; readonly order: number }"],
  settle: ["{ id: string }", "MadoReceipt"],
  postcondition: ["{ observation: MadoObservation; checkpoint: MadoObservation; expected: string }", "{ readonly satisfied: boolean; readonly frame: number; readonly checkpoint: number }"],
  release: ["{ id: string }", "{ readonly released: true }"],
  wait: ["{ duration_ms: number }", "{ readonly elapsed_ms: number }"],
  log: ["{ message: string }", "{ readonly recorded: boolean }"],
});

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
  return `// Generated from the captured schema and application-owned mado-host-v1 contract.
type MadoOptions = ${optionType(schema)};
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
${Object.entries(methods).map(([method, [args, result]]) => `  ${method}: { args: ${args}; result: ${result} };`).join("\n")}
}
declare const host: {
  readonly options: MadoOptions;
  readonly state: Record<string, unknown>;
  call<K extends keyof MadoCalls>(method: K, args: MadoCalls[K]["args"]): MadoCalls[K]["result"];
};
`;
}

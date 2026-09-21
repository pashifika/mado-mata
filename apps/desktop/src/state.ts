import type {ControllerView, LogEntry, Schema, Json} from './types.ts';

export function acceptController(current:ControllerView, incoming:ControllerView, expectedRun:string|null):ControllerView {
  return expectedRun !== null && incoming.run !== expectedRun ? current : incoming;
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

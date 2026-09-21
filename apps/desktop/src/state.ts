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

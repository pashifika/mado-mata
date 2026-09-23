import type {LogEntry, NotificationPreferences} from './types.ts';

// Only attributable command/operation outcomes become cards; routine logs never do.
const OUTCOME_CODES: Record<string, true> = {
  'workspace.opened': true, 'workspace.reinspected': true, 'workspace.closed': true,
  'profile.saved': true, 'profile.renamed': true, 'profile.deleted': true,
  'settings.saved': true, 'command.failed': true, 'run.terminal': true,
};

export type Severity = 'success' | 'warning' | 'error';

export interface Card {
  // Immutable event identity; also the dedup key.
  id:number; severity:Severity; code:string; message:string; workspaceId:string|null; run:string|null;
  category:string|null; action:string|null;
  // Captured when created; a later preference change never alters a live card.
  timeoutMs:number; remainingMs:number; hovered:boolean; focused:boolean;
}

// Sequences are monotonic, so one high-water mark replaces an unbounded seen-id history.
export interface CardStack {cards:Card[]; lastSequence:number}
export const emptyStack: CardStack = {cards: [], lastSequence: 0};

export function severityOf(level:string):Severity {
  const normalized = level.toLowerCase();
  if (normalized === 'error') return 'error';
  if (normalized === 'warn' || normalized === 'warning') return 'warning';
  return 'success';
}

function safeField(fields:LogEntry['fields'], key:string):string|null {
  if (fields === null || typeof fields !== 'object' || Array.isArray(fields)) return null;
  const value = fields[key];
  return typeof value === 'string' && value !== '' ? value.slice(0, 64) : null;
}

export function ingestCards(stack:CardStack, entries:LogEntry[], preferences:NotificationPreferences):CardStack {
  let cards = stack.cards;
  let lastSequence = stack.lastSequence;
  for (const entry of entries) {
    if (entry.sequence <= lastSequence) continue;
    lastSequence = entry.sequence;
    if (OUTCOME_CODES[entry.code] !== true) continue;
    const severity = severityOf(entry.level);
    if (severity === 'success' && !preferences.show_success) continue;
    const timeoutMs = preferences.timeout_seconds * 1000;
    cards = [...cards, {
      id: entry.sequence, severity, code: entry.code, message: entry.message, workspaceId: entry.workspace_id, run: entry.run,
      category: safeField(entry.fields, 'category'), action: safeField(entry.fields, 'action'),
      timeoutMs, remainingMs: timeoutMs, hovered: false, focused: false,
    }];
  }
  if (cards === stack.cards && lastSequence === stack.lastSequence) return stack;
  return {cards: trimVisible(cards, preferences.visible_count), lastSequence};
}

// Overflow displaces the oldest visible card; there is no waiting queue.
function trimVisible(cards:Card[], count:number):Card[] {
  return cards.length > count ? cards.slice(cards.length - count) : cards;
}

export function trimCards(stack:CardStack, count:number):CardStack {
  const cards = trimVisible(stack.cards, count);
  return cards === stack.cards ? stack : {...stack, cards};
}

// Hover or focus pauses a card's own countdown; other cards keep expiring.
export function tickCards(stack:CardStack, elapsedMs:number):CardStack {
  if (stack.cards.length === 0 || elapsedMs <= 0) return stack;
  let changed = false;
  const cards:Card[] = [];
  for (const card of stack.cards) {
    if (card.hovered || card.focused) {cards.push(card); continue;}
    const remainingMs = card.remainingMs - elapsedMs;
    changed = true;
    if (remainingMs > 0) cards.push({...card, remainingMs});
  }
  return changed ? {...stack, cards} : stack;
}

export function dismissCard(stack:CardStack, id:number):CardStack {
  const cards = stack.cards.filter(card => card.id !== id);
  return cards.length === stack.cards.length ? stack : {...stack, cards};
}

export function interactCard(stack:CardStack, id:number, interaction:Partial<Pick<Card,'hovered'|'focused'>>):CardStack {
  const index = stack.cards.findIndex(card => card.id === id);
  if (index < 0) return stack;
  const current = stack.cards[index];
  const next = {...current, ...interaction};
  if (next.hovered === current.hovered && next.focused === current.focused) return stack;
  const cards = [...stack.cards];
  cards[index] = next;
  return {...stack, cards};
}

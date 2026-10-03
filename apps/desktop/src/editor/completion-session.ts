import type {EditorState, Transaction} from '@codemirror/state';
import {syntaxTree} from '@codemirror/language';
import type {EditInput} from '../authoring.ts';
import type {EditorCompletionPreferences} from '../types.ts';
import {activatesCompletion} from './completion-activation.ts';
import type {CompletionContext, CompletionResult} from './completion-types.ts';
import type {SourcePositions} from './source-positions.ts';

interface ViewSnapshot {
  state: EditorState;
  mapping: SourcePositions;
  context: CompletionContext | null;
  focused: boolean;
  composing: boolean;
  readOnly: boolean;
  preferences: EditorCompletionPreferences;
}
type RequestCause = 'automatic' | 'explicit' | 'refresh';
interface Boundary {from: number; quote: string | null}
interface RequestTicket {
  readonly cause: RequestCause;
  readonly snapshot: ViewSnapshot;
  readonly boundary: Boundary;
  context: CompletionContext | null;
  waiting: boolean;
  ready: boolean;
  dispatched: boolean;
}
export interface CompletionPublication {
  readonly ticket: RequestTicket;
  readonly result: CompletionResult;
}
interface Environment {
  read: () => ViewSnapshot;
  request: (position: number, explicit: boolean) => Promise<CompletionResult | null>;
  accepts: (result: CompletionResult, source: string) => boolean;
  close: () => void;
  publish: () => void;
  setTimer: (callback: () => void, ms: number) => number;
  clearTimer: (timer: number) => void;
}

function boundaryAt(state: EditorState): Boundary {
  const position = state.selection.main.head;
  for (let node = syntaxTree(state).resolveInner(position, -1); node.parent; node = node.parent) {
    if (node.name === 'Interpolation') break;
    if (node.name === 'String' || node.name === 'TemplateString'
      || (node.name === 'PropertyName' && /['"`]/.test(state.doc.sliceString(node.from, node.from + 1)))) {
      const quote = state.doc.sliceString(node.from, node.from + 1);
      let backslashes = 0;
      for (let offset = node.to - 2; offset > node.from && state.doc.sliceString(offset, offset + 1) === '\\'; offset -= 1) backslashes += 1;
      const closed = node.to > node.from + 1 && state.doc.sliceString(node.to - 1, node.to) === quote && backslashes % 2 === 0;
      if (position > node.from && position <= node.to - (closed ? 1 : 0)) return {from: node.from + 1, quote};
      break;
    }
    if (node.name === 'VariableName' || node.name === 'VariableDefinition' || node.name === 'PropertyName') {
      return {from: node.from, quote: null};
    }
  }
  return {from: state.wordAt(position)?.from ?? position, quote: null};
}

function sameScope(a: CompletionContext, b: CompletionContext): boolean {
  return a.owner === b.owner && a.path === b.path && a.schema === b.schema && a.otherBytes === b.otherBytes;
}
function sameContext(a: CompletionContext, b: CompletionContext): boolean {
  return sameScope(a, b) && a.revision === b.revision && a.generation === b.generation && a.source === b.source;
}
function directTyping(transaction: Transaction, input: EditInput | null): boolean {
  return input?.type === 'insertText' && !input.composing && transaction.isUserEvent('input.type')
    && !transaction.isUserEvent('input.type.compose');
}
function compatible(transaction: Transaction, input: EditInput | null, boundary: Boundary): boolean {
  const before = transaction.startState.selection.main, after = transaction.newSelection.main;
  if (!before.empty || !after.empty || input?.composing || transaction.isUserEvent('input.type.compose')) return false;
  let count = 0, matches = false;
  transaction.changes.iterChanges((from, to, _newFrom, newTo, inserted) => {
    count += 1;
    if (from < boundary.from || to !== before.head || after.head !== newTo) return;
    if (input?.type === 'deleteContentBackward' && transaction.isUserEvent('delete.backward')) {
      matches = from < to && inserted.length === 0;
    } else if (directTyping(transaction, input) && from === to && inserted.length > 0) {
      const text = inserted.toString();
      if (boundary.quote === null) matches = /^[$\p{ID_Continue}\\{}]+$/u.test(text);
      else {
        const next = boundaryAt(transaction.state);
        matches = !/[\r\n]/.test(text) && next.from === boundary.from && next.quote === boundary.quote;
      }
    }
  });
  return count === 1 && matches;
}

// One view owns intent; the client still owns worker lifetime, coalescing and response identities.
export class CompletionSession {
  private readonly environment: Environment;
  private preferences: EditorCompletionPreferences;
  private ticket: RequestTicket | null = null;
  private timer: number | null = null;
  private live = false;
  private publication: CompletionPublication | null = null;

  constructor(environment: Environment) {
    this.environment = environment;
    this.preferences = {...environment.read().preferences};
  }

  cancel(): boolean {
    const active = this.ticket !== null;
    if (this.timer !== null) this.environment.clearTimer(this.timer);
    this.timer = null;
    this.ticket = null;
    this.publication = null;
    this.live = false;
    this.environment.close();
    return active;
  }

  explicit(): void {
    this.cancel();
    this.begin('explicit', false);
  }

  // Called after the view's transactions, before publishing their edits to the React owner.
  update(transactions: readonly Transaction[], inputs: readonly (EditInput | null)[]): void {
    if (!transactions.some(transaction => transaction.docChanged || transaction.selection)) return;
    const previous = this.ticket;
    let continues = previous !== null;
    let typed = false;
    let position = previous?.snapshot.state.selection.main.head;
    for (let index = 0; index < transactions.length; index += 1) {
      const transaction = transactions[index]!;
      if (transaction.docChanged) {
        const input = inputs[index] ?? null;
        continues = continues && transaction.startState.selection.main.head === position
          && compatible(transaction, input, previous!.boundary);
        const nextPosition = transaction.newSelection.main.head;
        const last = transaction.newDoc.sliceString(Math.max(0, nextPosition - 1), nextPosition);
        typed = directTyping(transaction, input) && activatesCompletion(transaction)
          && (!/['"`]/.test(last) || boundaryAt(transaction.state).quote !== null);
        position = transaction.newSelection.main.head;
      } else if (transaction.selection) {
        continues = false;
        typed = false;
      }
    }
    const wasLive = this.live;
    this.cancel();
    if (wasLive && continues) this.begin('refresh', true, previous!.boundary);
    else if (typed && this.environment.read().preferences.automatic) this.begin('automatic', true);
  }

  // A local edit must first acquire the authoritative post-edit context. A generation change
  // without that admitted edit instead invalidates the session, even when the text is equal.
  synchronize(): void {
    const snapshot = this.environment.read();
    if (snapshot.preferences.automatic !== this.preferences.automatic || snapshot.preferences.delay_ms !== this.preferences.delay_ms) {
      this.preferences = {...snapshot.preferences};
      this.cancel();
      return;
    }
    const ticket = this.ticket;
    if (!ticket) return;
    if (!this.currentView(ticket, snapshot) || !snapshot.context || !ticket.context
      || !sameScope(ticket.context, snapshot.context)) { this.cancel(); return; }
    if (ticket.waiting) {
      if (sameContext(ticket.context, snapshot.context) || snapshot.context.source !== snapshot.mapping.source) return;
      ticket.context = snapshot.context;
      ticket.waiting = false;
    } else if (!sameContext(ticket.context, snapshot.context)) {
      this.cancel();
      return;
    }
    if (ticket.ready && !ticket.dispatched) this.dispatch(ticket);
  }

  current(): CompletionPublication | null {
    const publication = this.publication;
    return publication && this.accepts(publication) ? publication : null;
  }

  accepts(publication: CompletionPublication): boolean {
    const snapshot = this.environment.read();
    return publication === this.publication && publication.ticket === this.ticket
      && this.currentView(publication.ticket, snapshot) && snapshot.context !== null
      && publication.ticket.context !== null && sameContext(publication.ticket.context, snapshot.context)
      && this.environment.accepts(publication.result, snapshot.mapping.source);
  }

  private currentView(ticket: RequestTicket, snapshot: ViewSnapshot): boolean {
    const before = ticket.snapshot.state.selection.main, after = snapshot.state.selection.main;
    return snapshot.focused && !snapshot.composing && !snapshot.readOnly
      && snapshot.state.doc === ticket.snapshot.state.doc && snapshot.mapping === ticket.snapshot.mapping
      && before.anchor === after.anchor && before.head === after.head
      && snapshot.preferences.automatic === this.preferences.automatic && snapshot.preferences.delay_ms === this.preferences.delay_ms;
  }

  private begin(cause: RequestCause, waiting: boolean, boundary?: Boundary): void {
    const snapshot = this.environment.read();
    if (!snapshot.focused || snapshot.composing || snapshot.readOnly || !snapshot.context
      || (cause !== 'explicit' && !snapshot.state.selection.main.empty)) return;
    const ticket: RequestTicket = {cause, snapshot, boundary: boundary ?? boundaryAt(snapshot.state),
      context: snapshot.context, waiting, ready: cause !== 'automatic', dispatched: false};
    this.ticket = ticket;
    this.live = cause !== 'automatic';
    if (cause === 'automatic') {
      this.timer = this.environment.setTimer(() => {
        this.timer = null;
        if (this.ticket !== ticket) return;
        ticket.ready = true;
        this.synchronize();
      }, snapshot.preferences.delay_ms);
    } else {
      this.synchronize();
    }
  }

  private dispatch(ticket: RequestTicket): void {
    const snapshot = this.environment.read();
    if (ticket.waiting || snapshot.context?.source !== snapshot.mapping.source || !this.currentView(ticket, snapshot)) {
      this.cancel();
      return;
    }
    ticket.dispatched = true;
    this.live = true;
    // Retry authority is captured by this request, never inherited from the session's origin.
    void this.environment.request(snapshot.mapping.toSource(snapshot.state.selection.main.head), ticket.cause === 'explicit').then(result => {
      if (this.ticket !== ticket) return;
      if (!result || result.candidates.length === 0) { this.cancel(); return; }
      this.publication = {ticket, result};
      if (!this.accepts(this.publication)) { this.cancel(); return; }
      this.environment.publish();
    });
  }
}

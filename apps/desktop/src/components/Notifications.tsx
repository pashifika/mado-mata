import {useEffect, useRef} from 'react';
import type {Card} from '../notifications.ts';

const SEVERITY_LABEL: Record<Card['severity'], string> = {success: 'Success', warning: 'Warning', error: 'Error'};
const SEVERITY_SYMBOL: Record<Card['severity'], string> = {success: '✓', warning: '!', error: '×'};

interface Props {
  cards: Card[];
  scopeLabel: (workspaceId: string | null) => string;
  onDismiss: (id: number) => void;
  onOpen: (card: Card) => void;
  onInteract: (id: number, interaction: {hovered?: boolean; focused?: boolean}) => void;
}

export default function Notifications({cards, scopeLabel, onDismiss, onOpen, onInteract}: Props) {
  // The card that currently owns keyboard focus, kept apart from the store's pause flag: when × or a newer outcome
  // removes that card, focus is handed to the card now in its slot, else back to where focus entered the stack.
  const owned = useRef<number | null>(null);
  const returnTo = useRef<HTMLElement | null>(null);
  const previous = useRef(cards);
  useEffect(() => {
    const before = previous.current;
    previous.current = cards;
    const lost = owned.current;
    if (lost === null || cards.some(card => card.id === lost)) return;
    owned.current = null;
    // A removal that already placed focus elsewhere (View logs revealing its event, a modal) is left alone.
    if (document.activeElement !== null && document.activeElement !== document.body) return;
    const neighbor = cards[Math.min(before.findIndex(card => card.id === lost), cards.length - 1)];
    if (neighbor) {
      document.getElementById(`notification-${neighbor.id}`)?.querySelector<HTMLElement>('button.dismiss')?.focus();
      return;
    }
    const origin = returnTo.current;
    returnTo.current = null;
    (origin?.isConnected && !(origin as HTMLButtonElement).disabled ? origin : document.getElementById('application-menu'))?.focus();
  }, [cards]);
  if (cards.length === 0) return null;
  return <div id="notifications" className="toast-stack" role="region" aria-label="Notifications"
    onFocus={event => {
      const from = event.relatedTarget;
      if (from instanceof HTMLElement && !event.currentTarget.contains(from)) returnTo.current = from;
    }}>
    {cards.map(card => <article key={card.id} id={`notification-${card.id}`} className={`toast ${card.severity}`} role={card.severity === 'error' ? 'alert' : 'status'}
      onMouseEnter={() => onInteract(card.id, {hovered: true})} onMouseLeave={() => onInteract(card.id, {hovered: false})}
      onFocus={() => {owned.current = card.id; onInteract(card.id, {focused: true});}} onBlur={event => {
        if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
        // Focus moving to another element releases ownership. Without a destination, only a card that is still in the
        // document lost focus to a click elsewhere; a card blurred by its own removal keeps ownership for the hand-off.
        const element = event.currentTarget;
        if (event.relatedTarget !== null) {if (owned.current === card.id) owned.current = null;}
        else queueMicrotask(() => {if (element.isConnected && owned.current === card.id) owned.current = null;});
        onInteract(card.id, {focused: false});
      }}>
      <div className="toast-body">
        <span className="toast-symbol" aria-hidden="true">{SEVERITY_SYMBOL[card.severity]}</span>
        <div className="toast-content">
          <div className="toast-scope">{scopeLabel(card.workspaceId)} · {SEVERITY_LABEL[card.severity]}</div>
          <strong>{card.message}</strong>
          <p className="toast-tags"><code>{card.code}</code>{card.category && <span className="tag">{card.category}</span>}{card.action && <span className="tag">{card.action}</span>}</p>
        </div>
        <button type="button" className="dismiss" aria-label={`Dismiss notification ${card.id}`} onClick={() => onDismiss(card.id)}>×</button>
      </div>
      <div className="toast-footer">
        <span>Event #{card.id}{card.run ? <> · <code>{card.run}</code></> : ''}{card.hovered || card.focused ? ' · paused' : ''}</span>
        <button type="button" className="link" onClick={() => {owned.current = null; onOpen(card);}}>View logs</button>
      </div>
      <div className="toast-progress" style={{transform: `scaleX(${Math.max(0, card.remainingMs / card.timeoutMs)})`}} aria-hidden="true"/>
    </article>)}
  </div>;
}

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
  if (cards.length === 0) return null;
  return <div id="notifications" className="toast-stack" role="region" aria-label="Notifications">
    {cards.map(card => <article key={card.id} id={`notification-${card.id}`} className={`toast ${card.severity}`} role={card.severity === 'error' ? 'alert' : 'status'}
      onMouseEnter={() => onInteract(card.id, {hovered: true})} onMouseLeave={() => onInteract(card.id, {hovered: false})}
      onFocus={() => onInteract(card.id, {focused: true})} onBlur={event => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) onInteract(card.id, {focused: false});
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
        <button type="button" className="link" onClick={() => onOpen(card)}>View logs</button>
      </div>
      <div className="toast-progress" style={{transform: `scaleX(${Math.max(0, card.remainingMs / card.timeoutMs)})`}} aria-hidden="true"/>
    </article>)}
  </div>;
}

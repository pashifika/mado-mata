import {cloneElement, createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore} from 'react';
import type {ButtonHTMLAttributes, CSSProperties, ReactElement, ReactNode, RefObject} from 'react';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import './contextual-help.css';

export interface HelpTopic {title: string; content: ReactNode}
export interface HelpController {
  topic: HelpTopic | null;
  open: (topic: HelpTopic, opener: HTMLElement) => void;
  close: (restoreFocus?: boolean, expectedTopic?: HelpTopic) => void;
}

// Notify only the panel: publishing inline JSX through useHelp state would rerender
// its owner, create new JSX, and repeat the update indefinitely.
class LiveHelpTopic implements HelpTopic {
  private snapshot: HelpTopic;
  private readonly listeners = new Set<() => void>();

  constructor(title: string, content: ReactNode) {this.snapshot = {title, content};}
  get title() {return this.snapshot.title;}
  get content() {return this.snapshot.content;}
  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {this.listeners.delete(listener);};
  };
  update(title: string, content: ReactNode) {
    if (title === this.snapshot.title && content === this.snapshot.content) return;
    this.snapshot = {title, content};
    this.listeners.forEach(listener => listener());
  }
}

const subscribeStaticTopic = () => () => {};

const DEFAULT_PANEL_ID = 'workspace-help-panel';
const HelpContext = createContext<{controller: HelpController; panelId: string} | null>(null);

function modalOpen(): boolean {
  return Array.from(document.querySelectorAll<HTMLElement>('dialog[open], [role="dialog"][aria-modal="true"], [role="alertdialog"][aria-modal="true"]'))
    .some(element => element.getClientRects().length > 0 && getComputedStyle(element).visibility !== 'hidden');
}

function canFocus(element: HTMLElement | null): element is HTMLElement {
  return element !== null && element.isConnected && !element.matches(':disabled, [aria-disabled="true"]')
    && !element.closest('[inert], [hidden], [aria-hidden="true"]') && element.getClientRects().length > 0
    && getComputedStyle(element).visibility !== 'hidden';
}

export function useHelp(scopeKey: string, onOpen?: () => void): HelpController {
  const [active, setActive] = useState<{scopeKey: string; topic: HelpTopic} | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const activeTopic = useRef<HelpTopic | null>(null);
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;
  const topic = active?.scopeKey === scopeKey ? active.topic : null;

  const close = useCallback((restoreFocus = true, expectedTopic?: HelpTopic) => {
    if (expectedTopic !== undefined && activeTopic.current !== expectedTopic) return;
    const previous = opener.current;
    const panel = previous && document.getElementById(previous.getAttribute('aria-controls') ?? DEFAULT_PANEL_ID);
    const focused = document.activeElement;
    const ownsFocus = focused === previous || (focused !== null && panel?.contains(focused));
    opener.current = null;
    activeTopic.current = null;
    setActive(null);
    if (!restoreFocus || !ownsFocus || previous === null || modalOpen()) return;
    // React restores pre-commit focus after deletion cleanups; restore only after that commit.
    queueMicrotask(() => {
      if (activeTopic.current !== null || modalOpen()) return;
      if (document.activeElement !== focused && document.activeElement !== document.body) return;
      const target = [expectedTopic === undefined ? previous : null, document.getElementById('workspace-help'), document.getElementById('page-run')]
        .find(canFocus);
      target?.focus();
    });
  }, []);
  const open = useCallback((next: HelpTopic, trigger: HTMLElement) => {
    opener.current = trigger;
    activeTopic.current = next;
    onOpenRef.current?.();
    setActive({scopeKey, topic: next});
  }, [scopeKey]);

  useLayoutEffect(() => {close(false);}, [scopeKey, close]);
  useEffect(() => {
    if (topic === null) return;
    // Bubble after editor/menu handlers, and leave native dialog cancellation to its owner.
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || modalOpen()) return;
      const target = event.target;
      if (target instanceof Element && target.closest('.cm-editor, input, textarea, select, [contenteditable="true"], [role="combobox"], [role="listbox"], [role="menu"], [role="alertdialog"]')) return;
      event.preventDefault();
      event.stopPropagation();
      close();
    };
    document.addEventListener('keydown', escape);
    return () => document.removeEventListener('keydown', escape);
  }, [topic, close]);

  return useMemo(() => ({topic, open, close}), [topic, open, close]);
}

export function HelpProvider({controller, children, panelId = DEFAULT_PANEL_ID}: {
  controller: HelpController; children: ReactNode; panelId?: string;
}) {
  const value = useMemo(() => ({controller, panelId}), [controller, panelId]);
  return <HelpContext value={value}>{children}</HelpContext>;
}

function useHint(text: string, anchor: RefObject<HTMLElement | null>) {
  const element = useRef<HTMLSpanElement>(null);
  const id = useId();
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<CSSProperties>({left: 12, top: 12});
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!anchor.current || !element.current) return;
      const bounds = anchor.current.getBoundingClientRect();
      const box = element.current.getBoundingClientRect();
      const width = document.documentElement.clientWidth;
      const height = document.documentElement.clientHeight;
      const left = Math.max(12, Math.min(bounds.left + (bounds.width - box.width) / 2, width - box.width - 12));
      const above = bounds.top - box.height - 6;
      const top = Math.max(12, above >= 12 ? above : Math.min(bounds.bottom + 6, height - box.height - 12));
      setPosition(current => current.left === left && current.top === top ? current : {left, top});
    };
    place();
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    return () => {
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
    };
  }, [open, text, anchor]);
  return {
    id, hide: () => setOpen(false),
    events: {
      onMouseEnter: () => setOpen(true), onMouseLeave: () => setOpen(false),
      onFocus: () => setOpen(true), onBlur: () => setOpen(false),
    },
    tooltip: <span ref={element} id={id} className="contextual-help-hint" role="tooltip" hidden={!open} style={position}>{text}</span>,
  };
}

export function ButtonHint({hint, children}: {
  hint: string; children: ReactElement<ButtonHTMLAttributes<HTMLButtonElement>>;
}) {
  const anchor = useRef<HTMLSpanElement>(null);
  const description = useHint(children.props.title || hint, anchor);
  return <span ref={anchor} className="button-hint" {...description.events}>
    {cloneElement(children, {
      title: undefined,
      'aria-describedby': [children.props['aria-describedby'], description.id].filter(Boolean).join(' '),
    })}
    {description.tooltip}
  </span>;
}

export function HelpTrigger({title, hint, children, id, label}: {
  title: string; hint: string; children: ReactNode; id?: string; label?: string;
}) {
  const context = useContext(HelpContext);
  if (context === null) throw new Error('HelpTrigger requires HelpProvider');
  const {controller, panelId} = context;
  const locale = useLocale();
  const helpLabel = messages[locale].ui.recognition.previewHelpButton;
  const trigger = useRef<HTMLButtonElement>(null);
  const description = useHint(hint, trigger);
  const [liveTopic] = useState(() => new LiveHelpTopic(title, children));
  const expanded = controller.topic === liveTopic;

  useLayoutEffect(() => {liveTopic.update(title, children);}, [liveTopic, title, children]);
  const close = controller.close;
  useLayoutEffect(() => () => {close(true, liveTopic);}, [close, liveTopic]);


  return <button ref={trigger} id={id} type="button"
    className={`contextual-help-trigger ${label === undefined ? 'contextual-help-icon-button' : 'contextual-help-text'}`}
    aria-label={`${helpLabel} · ${title}`} aria-describedby={description.id} aria-expanded={expanded} aria-controls={panelId}
    {...description.events}
    onClick={event => {
      // Keep Help activation independent of the adjacent field or action.
      event.preventDefault();
      event.stopPropagation();
      description.hide();
      if (expanded) {controller.close(); return;}
      controller.open(liveTopic, event.currentTarget);
    }}>
    {label ?? <span className="contextual-help-icon" aria-hidden="true">i</span>}
    {description.tooltip}
  </button>;
}

export function HelpPanel({controller, id, className, title, children}: {
  controller: HelpController; id?: string; className?: string; title?: string; children?: ReactNode;
}) {
  const context = useContext(HelpContext);
  const panelId = id ?? context?.panelId ?? DEFAULT_PANEL_ID;
  const panel = useRef<HTMLElement>(null);
  const liveTopic = controller.topic instanceof LiveHelpTopic ? controller.topic : null;
  const topic = useSyncExternalStore(liveTopic?.subscribe ?? subscribeStaticTopic,
    liveTopic?.getSnapshot ?? (() => controller.topic));
  const locale = useLocale();
  const closeLabel = messages[locale].ui.common.close;
  useLayoutEffect(() => {
    if (controller.topic !== null && !modalOpen()) panel.current?.focus({preventScroll: true});
  }, [controller.topic]);
  if (topic === null) return null;
  return <section ref={panel} id={panelId} className={`contextual-help-panel${className ? ` ${className}` : ''}`}
    aria-labelledby={`${panelId}-title`} tabIndex={-1}>
    <header className="contextual-help-heading">
      <h3 id={`${panelId}-title`}>{title ?? topic.title}</h3>
      <button type="button" onClick={() => controller.close()}>{closeLabel}</button>
    </header>
    <div className="contextual-help-content">{children === undefined ? topic.content : children}</div>
  </section>;
}

import {Fragment, useLayoutEffect, useRef, useState} from 'react';
import type {KeyboardEvent, MouseEvent} from 'react';
import {createPortal} from 'react-dom';

// Viewport coordinates the menu opens beside: below `bottom`, or above `top` when it does not fit below.
export interface MenuAnchor {left: number; top: number; bottom: number}

export interface MenuAction {
  id: string; label: string; onSelect: () => void;
  // Why the action is unavailable; a blocked item stays focusable with its reason but does nothing.
  blocked?: string | null;
  danger?: boolean;
  // Draws a separator before the item.
  separated?: boolean;
}

interface Props {
  id: string; label: string; anchor: MenuAnchor; actions: readonly MenuAction[];
  // Shown above the actions so the operator sees which row the menu acts on.
  heading?: string;
  // The visible button that toggles this menu; pressing it is not an outside press.
  toggle: HTMLElement | null;
  // `restoreFocus` is true for Escape, Tab and chosen actions, false for an outside press or a moved window.
  onClose: (restoreFocus: boolean) => void;
}

const EDGE = 8;

export function elementAnchor(element: Element): MenuAnchor {
  const rect = element.getBoundingClientRect();
  return {left: rect.left, top: rect.top, bottom: rect.bottom};
}

export function pointAnchor(x: number, y: number): MenuAnchor {
  return {left: x, top: y, bottom: y};
}

// Shift+F10 and the ContextMenu key open a row's menu from the keyboard.
export function menuKey(event: KeyboardEvent): boolean {
  return event.key === 'ContextMenu' || (event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey && event.key === 'F10');
}

// A context menu for an element: the pointer position for right-click, the element itself for keyboard-invoked
// context menus (which report no pointer position) and for Shift+F10. The browser's own menu never opens here.
export function menuEvents(open: (anchor: MenuAnchor, opener: HTMLElement) => void) {
  return {
    onContextMenu(event: MouseEvent<HTMLElement>) {
      event.preventDefault();
      event.stopPropagation();
      const element = event.currentTarget;
      open(event.clientX === 0 && event.clientY === 0 ? elementAnchor(element) : pointAnchor(event.clientX, event.clientY), element);
    },
    onKeyDown(event: KeyboardEvent<HTMLElement>) {
      if (!menuKey(event)) return;
      event.preventDefault();
      event.stopPropagation();
      open(elementAnchor(event.currentTarget), event.currentTarget);
    },
  };
}

// A fixed-position action menu kept inside the viewport. The first available action takes focus; arrow keys, Home
// and End move between actions, and Escape or Tab closes it. A press or focus outside, scrolling, resizing and
// leaving the window close it without taking focus back.
export default function ContextMenu({id, label, anchor, actions, heading, toggle, onClose}: Props) {
  const panel = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  const [place, setPlace] = useState<{left: number; top: number; maxHeight: number} | null>(null);

  // Re-measured after every render: a changed reason can resize the menu.
  useLayoutEffect(() => {
    const element = panel.current;
    const win = element?.ownerDocument.defaultView;
    if (!element || !win) return;
    const width = element.ownerDocument.documentElement.clientWidth;
    const height = win.innerHeight;
    const maxHeight = Math.max(0, height - 2 * EDGE);
    const box = Math.min(element.scrollHeight + element.offsetHeight - element.clientHeight, maxHeight);
    const below = anchor.bottom + 2;
    const preferred = below + box <= height - EDGE ? below : anchor.top - 2 - box;
    const next = {
      left: Math.max(EDGE, Math.min(anchor.left, width - EDGE - element.offsetWidth)),
      top: Math.max(EDGE, Math.min(preferred, height - EDGE - box)),
      maxHeight,
    };
    setPlace(previous => previous && previous.left === next.left && previous.top === next.top && previous.maxHeight === next.maxHeight ? previous : next);
  });

  const placed = place !== null;
  useLayoutEffect(() => {
    if (!placed) return;
    const items = Array.from(panel.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    (items.find(item => item.getAttribute('aria-disabled') !== 'true') ?? items[0])?.focus({preventScroll: true});
  }, [placed]);

  useLayoutEffect(() => {
    const element = panel.current;
    const doc = element?.ownerDocument;
    const win = doc?.defaultView;
    if (!element || !doc || !win) return;
    function outside(event: Event) {
      const target = event.target;
      if (target instanceof Node && (element!.contains(target) || toggle?.contains(target))) return;
      close.current(false);
    }
    function scroll(event: Event) {
      if (!(event.target instanceof Node) || !element!.contains(event.target)) close.current(false);
    }
    function leave() { close.current(false); }
    doc.addEventListener('pointerdown', outside, true);
    doc.addEventListener('focusin', outside, true);
    doc.addEventListener('scroll', scroll, true);
    win.addEventListener('resize', leave);
    win.addEventListener('blur', leave);
    return () => {
      doc.removeEventListener('pointerdown', outside, true);
      doc.removeEventListener('focusin', outside, true);
      doc.removeEventListener('scroll', scroll, true);
      win.removeEventListener('resize', leave);
      win.removeEventListener('blur', leave);
    };
  }, [toggle]);

  function keys(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === 'Escape' || event.key === 'Tab' || menuKey(event)) {
      event.preventDefault();
      event.stopPropagation();
      onClose(true);
      return;
    }
    const items = Array.from(panel.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    if (items.length === 0) return;
    const index = items.indexOf(event.target as HTMLElement);
    let next: number;
    if (event.key === 'ArrowDown') next = (index + 1) % items.length;
    else if (event.key === 'ArrowUp') next = index <= 0 ? items.length - 1 : index - 1;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = items.length - 1;
    else return;
    event.preventDefault();
    items[next].focus({preventScroll: true});
  }

  return createPortal(<div ref={panel} id={id} className="context-menu" role="menu" aria-label={label}
    style={place ?? {left: 0, top: 0, visibility: 'hidden'}} onKeyDown={keys} onContextMenu={event => event.preventDefault()}>
    {heading && <div className="menu-heading mono" aria-hidden="true">{heading}</div>}
    {actions.map(action => <Fragment key={action.id}>
      {action.separated && <div className="menu-separator" role="separator"/>}
      <button id={action.id} type="button" role="menuitem" className={action.danger ? 'danger' : undefined} aria-disabled={action.blocked ? true : undefined}
        onClick={() => {
          if (action.blocked) return;
          onClose(true);
          action.onSelect();
        }}>
        <span>{action.label}</span>
        {action.blocked && <span className="menu-reason">{action.blocked}</span>}
      </button>
    </Fragment>)}
  </div>, document.body);
}

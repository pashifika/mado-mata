import {useLayoutEffect, useRef, useState} from 'react';
import type {AriaAttributes, KeyboardEvent, ReactNode} from 'react';
import {createPortal, flushSync} from 'react-dom';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
  description?: ReactNode;
  title?: string;
}

interface Props extends Pick<AriaAttributes, 'aria-label' | 'aria-labelledby' | 'aria-describedby' | 'aria-invalid'> {
  id: string;
  value: string;
  options: readonly SelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  placeholder?: string;
  className?: string;
  title?: string;
  listLabel?: string;
  heading?: ReactNode;
  commitOnTab?: boolean;
  // Footer actions after the listbox; Tab walks them in order and Shift+Tab returns to the selected option.
  actions?: readonly SelectAction[];
}

export interface SelectAction {id: string; label: ReactNode; onClick: () => void; disabled?: boolean; title?: string; ariaLabel?: string}

interface Placement {left: number; top: number; width: number; maxHeight: number}

export default function Select({id, value, options, onChange, disabled = false, placeholder, className, title,
  listLabel, heading, commitOnTab = true, actions = [], ...aria}: Props) {
  const listId = `${id}-options`;
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const footers = useRef<(HTMLButtonElement | null)[]>([]);
  const reposition = useRef<(() => void) | null>(null);
  const search = useRef({text: '', time: 0});
  const [menu, setMenu] = useState<{host: HTMLElement; value: string} | null>(null);
  const [placement, setPlacement] = useState<Placement | null>(null);
  const [activeValue, setActiveValue] = useState<string | null>(null);
  const open = menu !== null && menu.value === value && !disabled;
  const selected = options.find(option => option.value === value);
  const activeIndex = activeValue === null ? -1 : options.findIndex(option => option.value === activeValue && !option.disabled);
  const selectedValue = selected && !selected.disabled ? selected.value : null;

  function dismiss(restoreFocus = false) {
    setMenu(null);
    search.current = {text: '', time: 0};
    if (restoreFocus) trigger.current?.focus({preventScroll: true});
  }

  function show(next = selectedValue) {
    const button = trigger.current;
    if (disabled || !button) return;
    search.current = {text: '', time: 0};
    setActiveValue(next);
    setPlacement(null);
    // A body portal would be inert and behind a native modal dialog's top layer.
    setMenu({host: button.closest('dialog') ?? button.ownerDocument.body, value});
    button.focus({preventScroll: true});
  }

  function commit(option: SelectOption | undefined, restoreFocus = true) {
    if (disabled) return;
    flushSync(() => dismiss(restoreFocus));
    if (option && !option.disabled && option.value !== value) onChange(option.value);
  }

  useLayoutEffect(() => {
    if (menu && !open) dismiss();
  }, [menu, open]);

  useLayoutEffect(() => {
    const button = trigger.current;
    const panel = popup.current;
    const scroller = list.current;
    if (!open || !menu || !button || !panel || !scroller) return;
    const doc = button.ownerDocument;
    const win = doc.defaultView;
    if (!win) return;
    const viewport = win.visualViewport;

    function place() {
      if (!button || !panel || !scroller || !win) return;
      const rect = button.getBoundingClientRect();
      const leftEdge = (viewport?.offsetLeft ?? 0) + 8;
      const topEdge = (viewport?.offsetTop ?? 0) + 8;
      const usableWidth = Math.max(0, (viewport?.width ?? doc.documentElement.clientWidth) - 16);
      const usableHeight = Math.max(0, (viewport?.height ?? win.innerHeight) - 16);
      const width = Math.min(Math.max(rect.width, 240), usableWidth);
      // Measure wrapping at the final width before choosing the opening direction.
      panel.style.width = `${width}px`;
      const naturalHeight = panel.getBoundingClientRect().height + scroller.scrollHeight - scroller.clientHeight;
      const below = Math.max(0, Math.min(usableHeight, topEdge + usableHeight - rect.bottom - 6));
      const above = Math.max(0, Math.min(usableHeight, rect.top - 6 - topEdge));
      const upward = below < Math.min(naturalHeight, 360) && above > below;
      const maxHeight = Math.min(360, upward ? above : below);
      const height = Math.min(naturalHeight, maxHeight);
      const next = {
        left: Math.max(leftEdge, Math.min(rect.left, leftEdge + usableWidth - width)),
        top: Math.max(topEdge, Math.min(upward ? rect.top - 6 - height : rect.bottom + 6, topEdge + usableHeight - height)),
        width, maxHeight,
      };
      setPlacement(previous => previous && previous.left === next.left && previous.top === next.top
        && previous.width === next.width && previous.maxHeight === next.maxHeight ? previous : next);
    }

    function outside(event: Event) {
      const target = event.target;
      if (target instanceof Node && !button?.contains(target) && !panel?.contains(target)) dismiss();
    }
    function scroll(event: Event) {
      if (!(event.target instanceof Node) || !panel?.contains(event.target)) place();
    }
    function close() { dismiss(); }

    reposition.current = place;
    const observer = new ResizeObserver(place);
    observer.observe(button);
    observer.observe(panel);
    observer.observe(scroller);
    doc.addEventListener('pointerdown', outside, true);
    doc.addEventListener('focusin', outside, true);
    doc.addEventListener('scroll', scroll, true);
    win.addEventListener('resize', place);
    win.addEventListener('blur', close);
    viewport?.addEventListener('resize', place);
    viewport?.addEventListener('scroll', place);
    menu.host.addEventListener('close', close);
    return () => {
      reposition.current = null;
      observer.disconnect();
      doc.removeEventListener('pointerdown', outside, true);
      doc.removeEventListener('focusin', outside, true);
      doc.removeEventListener('scroll', scroll, true);
      win.removeEventListener('resize', place);
      win.removeEventListener('blur', close);
      viewport?.removeEventListener('resize', place);
      viewport?.removeEventListener('scroll', place);
      menu.host.removeEventListener('close', close);
    };
  }, [menu, open]);

  // Content above the trigger can move it without resizing either element.
  useLayoutEffect(() => {reposition.current?.();});

  useLayoutEffect(() => {
    const scroller = list.current;
    const row = scroller?.children[activeIndex] as HTMLElement | undefined;
    if (!open || !placement || !scroller || !row) return;
    // scrollIntoView can also scroll the settings dialog and the document.
    const bottom = row.offsetTop + row.offsetHeight;
    if (row.offsetTop < scroller.scrollTop || row.offsetHeight > scroller.clientHeight) scroller.scrollTop = row.offsetTop;
    else if (bottom > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = bottom - scroller.clientHeight;
  }, [open, activeIndex, activeValue, placement]);

  function enabledIndex(start: number, step: number) {
    for (let index = start; index >= 0 && index < options.length; index += step) {
      if (!options[index].disabled) return index;
    }
    return -1;
  }

  function enabledFooter(start: number, step: number) {
    for (let index = start; index >= 0 && index < actions.length; index += step) {
      const button = footers.current[index];
      if (button && !button.disabled) return button;
    }
    return null;
  }

  function keys(event: KeyboardEvent<HTMLButtonElement>) {
    if (disabled || event.nativeEvent.isComposing) return;
    if (event.key === 'Escape' && open) {
      event.preventDefault();
      event.stopPropagation();
      dismiss(true);
      return;
    }
    if (event.key === 'Tab' && open) {
      const first = enabledFooter(0, 1);
      if (!commitOnTab && !event.shiftKey && first) {
        event.preventDefault();
        first.focus({preventScroll: true});
      } else if (commitOnTab) commit(options[activeIndex], false);
      else flushSync(() => dismiss());
      return;
    }
    if (event.ctrlKey || event.metaKey) return;
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (!event.repeat) {
        if (open) commit(options[activeIndex]);
        else show();
      }
      return;
    }
    let next = -1;
    if (event.key === 'Home') next = enabledIndex(0, 1);
    else if (event.key === 'End') next = enabledIndex(options.length - 1, -1);
    else if (event.key === 'ArrowDown') next = open ? enabledIndex(activeIndex + 1, 1)
      : selectedValue === null ? enabledIndex(0, 1) : options.findIndex(option => option.value === selectedValue);
    else if (event.key === 'ArrowUp') next = open ? enabledIndex(activeIndex < 0 ? options.length - 1 : activeIndex - 1, -1)
      : selectedValue === null ? enabledIndex(options.length - 1, -1) : options.findIndex(option => option.value === selectedValue);
    else if (event.key.length === 1 && !event.altKey) {
      event.preventDefault();
      const time = performance.now();
      const letter = event.key.toLocaleLowerCase();
      const prefix = time - search.current.time < 600 ? search.current.text + letter : letter;
      let query = Array.from(prefix).every(character => character === letter) ? letter : prefix;
      const current = open ? activeIndex : options.findIndex(option => option.value === selectedValue && !option.disabled);
      function match(text: string) {
        const start = text.length === 1 ? current + 1 : Math.max(current, 0);
        for (let offset = 0; offset < options.length; offset++) {
          const index = (start + offset) % options.length;
          if (!options[index].disabled && options[index].label.toLocaleLowerCase().startsWith(text)) return index;
        }
        return -1;
      }
      next = match(query);
      if (next < 0 && query.length > 1) {
        query = letter;
        next = match(query);
      }
      if (!open) show(next < 0 ? selectedValue : options[next].value);
      else if (next >= 0) setActiveValue(options[next].value);
      search.current = {text: query, time};
      return;
    } else return;
    event.preventDefault();
    search.current = {text: '', time: 0};
    if (!open) show(next < 0 ? null : options[next].value);
    else if (next >= 0) setActiveValue(options[next].value);
  }

  return <div className={className ? `select ${className}` : 'select'}>
    <button id={id} ref={trigger} type="button" className="select-trigger" role="combobox" disabled={disabled}
      aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? listId : undefined}
      aria-activedescendant={open && placement && activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined}
      {...aria} title={title} onKeyDown={keys} onClick={() => {if (!disabled) {if (open) dismiss(true); else show();}}}>
      <span className="select-label">{selected?.label ?? placeholder ?? value}</span>
      <span className="select-chevron" aria-hidden="true"/>
    </button>
    {open && menu && createPortal(<div ref={popup} className="select-popup"
      style={placement ?? {left: 0, top: 0, maxHeight: 360, visibility: 'hidden'}}
      onPointerDown={event => {
        if (event.button === 0 && !footers.current.some(button => button?.contains(event.target as Node))) event.preventDefault();
      }}>
      {heading != null && <div className="select-heading">{heading}</div>}
      <div id={listId} ref={list} className="select-options" role="listbox"
        aria-label={listLabel ?? aria['aria-label']}
        aria-labelledby={listLabel !== undefined || aria['aria-label'] !== undefined ? undefined : aria['aria-labelledby'] ?? id}>
        {options.map((option, index) => <div id={`${listId}-${index}`} key={option.value} className="select-option" role="option"
          aria-selected={option.value === value} aria-disabled={option.disabled || undefined}
          data-active={index === activeIndex ? 'true' : undefined} title={option.title}
          onPointerMove={event => {if (!option.disabled && event.pointerType !== 'touch') setActiveValue(option.value);}}
          onClick={() => {if (!option.disabled) commit(option);}}>
          <span className="select-option-text"><span className="select-option-label">{option.label}</span>
            {option.description != null && <span className="select-option-description">{option.description}</span>}
          </span>
          {option.value === value && <span className="select-selected-mark" aria-hidden="true"/>}
        </div>)}
      </div>
      {actions.length > 0 && <div className="select-actions">
        {actions.map((action, index) => <button key={action.id} id={action.id} ref={element => {footers.current[index] = element;}} type="button" className="select-action" tabIndex={-1}
          disabled={action.disabled} title={action.title} aria-label={action.ariaLabel}
          onClick={() => {
            if (disabled || action.disabled) return;
            flushSync(() => dismiss(true));
            action.onClick();
          }} onKeyDown={event => {
            if (event.key === 'Escape') {
              event.preventDefault();
              event.stopPropagation();
              dismiss(true);
            } else if (event.key === 'Tab') {
              const next = event.shiftKey ? enabledFooter(index - 1, -1) : enabledFooter(index + 1, 1);
              if (next) {
                event.preventDefault();
                next.focus({preventScroll: true});
              } else if (event.shiftKey) {
                event.preventDefault();
                setActiveValue(selectedValue);
                search.current = {text: '', time: 0};
                trigger.current?.focus({preventScroll: true});
              } else {
                // Resume native tab order at the trigger, not the portal's DOM position.
                flushSync(() => dismiss(true));
              }
            }
          }}>{action.label}</button>)}
      </div>}
    </div>, menu.host)}
  </div>;
}

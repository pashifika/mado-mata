import {useLayoutEffect, useRef, useState} from 'react';
import type {KeyboardEvent} from 'react';
import {createPortal, flushSync} from 'react-dom';
import {canonicalLanguageKey, languageCandidates} from '../i18n-language.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  id: string; value: string; placeholder: string; invalid: boolean; describedBy: string | undefined;
  // Every change belongs to the caller's draft at once: typed text as typed, a deliberate commit as its canonical key.
  onChange: (key: string) => void;
}

interface Placement {left: number; top: number; width: number; maxHeight: number}

// An editable language-key combobox (list autocomplete with manual selection) for the left side of a joined
// `input-select-group`. Typing filters the i18n-owned suggestions without choosing one; Arrow keys mark a suggestion
// and Enter, Tab or a click accepts it. Without a marked suggestion, Enter (and Tab after typing) commits the typed key
// in canonical form when it is a valid key, so keys outside the suggestions are accepted. Focusing or leaving the field
// without that commit never rewrites the key.
export default function LanguageKeyInput({id, value, placeholder, invalid, describedBy, onChange}: Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const r = t.releaseMetadata;
  const listId = `${id}-suggestions`;
  const field = useRef<HTMLInputElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const reposition = useRef<(() => void) | null>(null);
  // Typed since this field was focused or last committed: only then does Tab normalize.
  const edited = useRef(false);
  const [host, setHost] = useState<HTMLElement | null>(null);
  const [active, setActive] = useState(-1);
  const [placement, setPlacement] = useState<Placement | null>(null);
  const options = languageCandidates(t.languageKeys, value);
  const open = host !== null;
  const shown = open && options.length > 0;
  const marked = shown && active >= 0 && active < options.length ? active : -1;

  function show() {
    const input = field.current;
    if (open || !input) return;
    setPlacement(null);
    // A body portal would be inert and behind a native modal dialog's top layer.
    setHost(input.closest('dialog') ?? input.ownerDocument.body);
  }

  function dismiss() {
    setHost(null);
    setActive(-1);
  }

  function commit(key: string) {
    edited.current = false;
    flushSync(dismiss);
    if (key !== value) onChange(key);
  }

  function normalize() {
    const canonical = canonicalLanguageKey(value);
    commit(canonical ?? value);
  }

  useLayoutEffect(() => {
    const input = field.current;
    const panel = popup.current;
    const scroller = list.current;
    if (!shown || !input || !panel || !scroller) return;
    const doc = input.ownerDocument;
    const win = doc.defaultView;
    if (!win) return;
    function place() {
      if (!input || !panel || !scroller || !win) return;
      // The suggestions belong to the whole joined row, so they span it.
      const box = (input.closest('.input-select-group') ?? input).getBoundingClientRect();
      const usableWidth = Math.max(0, doc.documentElement.clientWidth - 16);
      const width = Math.min(Math.max(box.width, 240), usableWidth);
      panel.style.width = `${width}px`;
      const natural = panel.getBoundingClientRect().height + scroller.scrollHeight - scroller.clientHeight;
      const below = Math.max(0, win.innerHeight - box.bottom - 14);
      const above = Math.max(0, box.top - 14);
      const upward = below < Math.min(natural, 280) && above > below;
      const maxHeight = Math.min(280, upward ? above : below);
      const height = Math.min(natural, maxHeight);
      const next = {left: Math.max(8, Math.min(box.left, 8 + usableWidth - width)), top: upward ? box.top - 6 - height : box.bottom + 6, width, maxHeight};
      setPlacement(previous => previous && previous.left === next.left && previous.top === next.top
        && previous.width === next.width && previous.maxHeight === next.maxHeight ? previous : next);
    }
    function scroll(event: Event) {
      if (!(event.target instanceof Node) || !panel?.contains(event.target)) place();
    }
    reposition.current = place;
    const observer = new ResizeObserver(place);
    observer.observe(input);
    observer.observe(panel);
    doc.addEventListener('scroll', scroll, true);
    win.addEventListener('resize', place);
    win.addEventListener('blur', dismiss);
    return () => {
      reposition.current = null;
      observer.disconnect();
      doc.removeEventListener('scroll', scroll, true);
      win.removeEventListener('resize', place);
      win.removeEventListener('blur', dismiss);
    };
  }, [shown, host]);

  // Filtering changes the list height without resizing the observed elements.
  useLayoutEffect(() => {reposition.current?.();});

  useLayoutEffect(() => {
    const scroller = list.current;
    const row = marked < 0 ? undefined : scroller?.children[marked] as HTMLElement | undefined;
    if (!scroller || !row) return;
    // scrollIntoView would also scroll the Edit page.
    if (row.offsetTop < scroller.scrollTop) scroller.scrollTop = row.offsetTop;
    else if (row.offsetTop + row.offsetHeight > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = row.offsetTop + row.offsetHeight - scroller.clientHeight;
  }, [marked, placement]);

  function keys(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const down = event.key === 'ArrowDown';
      if (!open) show();
      // Alt+ArrowDown only opens the list, as in a native select.
      if (options.length === 0 || (event.altKey && !open)) return;
      setActive(marked < 0 ? (down ? 0 : options.length - 1) : (marked + (down ? 1 : options.length - 1)) % options.length);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      if (marked >= 0) commit(options[marked].key);
      else normalize();
    } else if (event.key === 'Tab') {
      // Focus moves on as usual; a marked suggestion or freshly typed key is committed first.
      if (marked >= 0) commit(options[marked].key);
      else if (edited.current) normalize();
      else dismiss();
    } else if (event.key === 'Escape' && open) {
      event.preventDefault();
      event.stopPropagation();
      dismiss();
    }
  }

  return <>
    <input id={id} ref={field} type="text" className="language-key-input" value={value} placeholder={placeholder}
      spellCheck={false} autoCapitalize="off" autoCorrect="off" autoComplete="off"
      role="combobox" aria-autocomplete="list" aria-expanded={shown} aria-controls={shown ? listId : undefined}
      aria-activedescendant={marked >= 0 && placement ? `${listId}-${marked}` : undefined}
      aria-invalid={invalid} aria-describedby={describedBy}
      onFocus={() => {edited.current = false;}}
      onClick={() => {if (!open) show();}}
      onChange={event => {
        edited.current = true;
        setActive(-1);
        show();
        onChange(event.target.value);
      }}
      onKeyDown={keys}
      onBlur={dismiss}/>
    <span className="visually-hidden" role="status">{open ? r.suggestionCount(options.length) : ''}</span>
    {shown && host && createPortal(<div ref={popup} className="select-popup language-key-popup"
      style={placement ?? {left: 0, top: 0, maxHeight: 280, visibility: 'hidden'}}
      // Choosing with the pointer keeps focus (and the caret) in the field.
      onPointerDown={event => event.preventDefault()}>
      <div id={listId} ref={list} className="select-options" role="listbox" aria-label={r.suggestions}>
        {options.map((option, index) => <div id={`${listId}-${index}`} key={option.key} className="select-option" role="option"
          aria-selected={index === marked} data-active={index === marked ? 'true' : undefined} data-key={option.key}
          onClick={() => commit(option.key)}>
          <span className="select-option-text"><span className="select-option-label"><span className="mono">{option.key}</span> · {option.label}</span></span>
        </div>)}
      </div>
    </div>, host)}
  </>;
}

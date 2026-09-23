import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent} from 'react';

export interface WorkspaceOption {
  id: string;
  label: string;
  path: string;
  status: {kind: 'ready' | 'busy' | 'dirty' | 'attention'; text: string};
  running: boolean;
  attention: boolean;
}

interface Props {
  items: WorkspaceOption[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

export default function WorkspaceSwitcher({items, selectedId, onSelect}: Props) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef({text: '', time: 0});
  const selected = items.find(item => item.id === selectedId);
  let running = 0;
  let errors = 0;
  for (const item of items) {
    if (item.running) running++;
    if (item.attention) errors++;
  }

  useEffect(() => {setOpen(false);}, [selectedId]);
  useEffect(() => {
    if (!open) return;
    search.current = {text: '', time: 0};
    const options = list.current?.querySelectorAll<HTMLElement>('[role="option"]');
    const target = Array.from(options ?? []).find(item => item.getAttribute('aria-selected') === 'true') ?? options?.[0];
    target?.focus();
    function outside(event: MouseEvent) {
      if (!anchor.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener('mousedown', outside);
    return () => document.removeEventListener('mousedown', outside);
  }, [open]);

  function close() {
    setOpen(false);
    trigger.current?.focus();
  }

  function choose(id: string) {
    close();
    onSelect(id);
  }

  function keys(event: KeyboardEvent<HTMLDivElement>) {
    const options = Array.from(list.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? []);
    const index = options.indexOf(document.activeElement as HTMLElement);
    if (event.key === 'Escape') {event.preventDefault(); close(); return;}
    if (event.key === 'Tab') {close(); return;}
    if ((event.key === 'Enter' || event.key === ' ') && index >= 0) {
      event.preventDefault();
      choose(items[index].id);
      return;
    }
    let next = -1;
    if (event.key === 'ArrowDown') next = Math.min(index + 1, options.length - 1);
    else if (event.key === 'ArrowUp') next = Math.max(index - 1, 0);
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = options.length - 1;
    else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      const time = performance.now();
      const letter = event.key.toLocaleLowerCase();
      const prefix = time - search.current.time < 600 ? search.current.text + letter : letter;
      search.current = {text: prefix, time};
      const query = Array.from(prefix).every(character => character === letter) ? letter : prefix;
      const start = query.length === 1 ? index + 1 : Math.max(index, 0);
      for (let offset = 0; offset < items.length; offset++) {
        const candidate = (start + offset) % items.length;
        if (items[candidate].label.toLocaleLowerCase().startsWith(query)) {next = candidate; break;}
      }
      event.preventDefault();
    }
    if (next >= 0) {
      event.preventDefault();
      options[next]?.focus();
    }
  }

  return <>
    <div className="workspace-summary" role="group" aria-label="All workspace status">
      <span id="workspace-running-count" className={running ? 'running-count active' : 'running-count'}>
        {running > 0 && <span className="workspace-status status-busy" aria-hidden="true"/>}Running <strong>{running}/{items.length}</strong>
      </span>
      {errors > 0 && <span id="workspace-error-count" className="workspace-error-count" title={`${errors} workspace${errors === 1 ? '' : 's'} need attention`}>Errors <strong>{errors}</strong></span>}
    </div>
    <div ref={anchor} className="workspace-picker" onBlur={event => {
      if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false);
    }}>
      <button id="workspace-select" ref={trigger} type="button" disabled={items.length === 0} aria-haspopup="listbox" aria-expanded={open}
        aria-controls="workspace-options" aria-describedby="workspace-summary" aria-label={`Workspace: ${selected?.label ?? 'Choose workspace'}`}
        title={selected?.path} onClick={() => setOpen(value => !value)} onKeyDown={event => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {event.preventDefault(); setOpen(true);}
        }}>
        <span className="workspace-selection-label">{selected?.label ?? (items.length ? 'Choose workspace' : 'No open workspaces')}</span>
        <span className="workspace-chevron" aria-hidden="true"/>
      </button>
      {open && <div className="workspace-popup">
        <div className="workspace-popup-heading"><span>Workspaces</span><span>{items.length} open</span></div>
        <div id="workspace-options" ref={list} role="listbox" aria-label="Workspaces" onKeyDown={keys}>
          {items.map(item => <button id={`workspace-option-${item.id}`} key={item.id} type="button" role="option" tabIndex={-1}
            aria-selected={item.id === selectedId} title={item.path} onClick={() => choose(item.id)}>
            <span className={`workspace-status status-${item.status.kind}`} aria-hidden="true">{item.status.kind === 'attention' ? '!' : ''}</span>
            <span className="workspace-option-text"><span className="workspace-option-label">{item.label}</span><span className={`workspace-option-status status-${item.status.kind}`}>{item.status.text}</span></span>
            {item.id === selectedId && <span className="workspace-selected-mark" aria-hidden="true"/>}
          </button>)}
        </div>
      </div>}
    </div>
  </>;
}

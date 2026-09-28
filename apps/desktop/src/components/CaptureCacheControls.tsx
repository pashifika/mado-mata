import {useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';
import type {Fault} from '../types.ts';
import {fault, FaultMessage} from './ResultPanel.tsx';

interface CacheInfo {folder:string; bytes:number|null; error:Fault|null}
export interface CaptureCacheControlsProps {
  disabled:boolean;
  enabled:boolean;
  onEnabled:(enabled:boolean) => void;
}

export default function CaptureCacheControls({disabled, enabled, onEnabled}:CaptureCacheControlsProps) {
  const locale = useLocale();
  const copy = messages[locale].ui.captureCache;
  const [expanded, setExpanded] = useState(false);
  const [info, setInfo] = useState<CacheInfo|null>(null);
  const [error, setError] = useState<Fault|null>(null);
  const [busy, setBusy] = useState(false);
  const [opened, setOpened] = useState(false);
  const generation = useRef(0);
  useEffect(() => () => { generation.current += 1; }, []);

  async function toggle() {
    const ticket = ++generation.current;
    setExpanded(!expanded);
    setOpened(false);
    setError(null);
    setInfo(null);
    if (expanded) { setBusy(false); return; }
    setBusy(true);
    try {
      const measured = await invoke<CacheInfo>('capture_cache_info');
      if (generation.current === ticket) { setInfo(measured); setError(measured.error); }
    } catch (cause) {
      if (generation.current === ticket) setError(fault(cause));
    } finally {
      if (generation.current === ticket) setBusy(false);
    }
  }

  async function openFolder() {
    const ticket = ++generation.current;
    setBusy(true);
    setError(null);
    setOpened(false);
    try {
      await invoke('capture_cache_open');
      if (generation.current === ticket) setOpened(true);
    } catch (cause) {
      if (generation.current === ticket) setError(fault(cause));
    } finally {
      if (generation.current === ticket) setBusy(false);
    }
  }

  return <section className="recognition-cache">
    <label><input type="checkbox" checked={enabled} disabled={disabled}
      onChange={event => onEnabled(event.target.checked)} />{copy.enable}</label>
    <p className="muted">{copy.privacy}</p>
    <div className="actions">
      <button type="button" aria-expanded={expanded} onClick={() => void toggle()}>{expanded ? copy.close : copy.manage}</button>
    </div>
    {expanded && <div>
      {info && <dl><dt>{copy.folder}</dt><dd><code>{info.folder}</code></dd>
        <dt>{copy.size}</dt><dd>{info.bytes === null ? copy.unavailable : info.bytes.toLocaleString(locale)}</dd></dl>}
      {busy && <p role="status">{copy.measuring}</p>}
      <button type="button" disabled={busy || disabled} onClick={() => void openFolder()}>{copy.open}</button>
      {opened && <p role="status">{copy.openAccepted}</p>}
      {error && <FaultMessage value={error} title={copy.failed} />}
    </div>}
  </section>;
}

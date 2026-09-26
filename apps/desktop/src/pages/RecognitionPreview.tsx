import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {emitTo, listen} from '@tauri-apps/api/event';
import RecognitionCanvas from '../components/RecognitionCanvas.tsx';
import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {LocaleContext} from '../locale.tsx';
import {PREVIEW_EDIT, PREVIEW_READY, PREVIEW_STATE, ZOOM_LEVELS, displayScale, stepZoom} from '../recognition.ts';
import type {PreviewDisplay, PreviewEdit, PreviewEditMessage, PreviewSnapshot} from '../recognition.ts';
import type {Fault} from '../types.ts';

// The main window's label in tauri.conf.json; it owns the Edit session and applies every relayed edit.
const MAIN_LABEL = 'main';

interface Raster {frameId: string; url: string}

// Root of the detached preview window: the frame image, zoom, content selection and on-image region editing.
// It holds no authoritative metadata. The main window emits snapshots; edits are relayed back and apply only there.
export default function RecognitionPreview() {
  const [snapshot, setSnapshot] = useState<PreviewSnapshot | null>(null);
  // False until the main window answered once; then null means no Edit session shares recognition state.
  const [received, setReceived] = useState(false);
  const [raster, setRaster] = useState<Raster | null>(null);
  const [rasterError, setRasterError] = useState<Fault | null>(null);
  const [sendError, setSendError] = useState<Fault | null>(null);
  const [stopError, setStopError] = useState<Fault | null>(null);
  const [display, setDisplay] = useState<PreviewDisplay>({zoom: 'fit', tool: 'zones'});
  const [viewport, setViewport] = useState({width: 0, height: 0});
  const stage = useRef<HTMLDivElement>(null);
  const locale = snapshot?.locale ?? 'en';
  const r = messages[locale].ui.recognition;

  useEffect(() => {
    let alive = true;
    let stop: (() => void) | null = null;
    void listen<PreviewSnapshot | null>(PREVIEW_STATE, event => {
      setSnapshot(event.payload);
      setReceived(true);
    }).then(unlisten => {
      if (!alive) {unlisten(); return;}
      stop = unlisten;
      return emitTo(MAIN_LABEL, PREVIEW_READY);
    }).catch(cause => setSendError(fault(cause)));
    return () => {alive = false; stop?.();};
  }, []);

  // The display choice lives in the main window so a reopened preview resumes it.
  useEffect(() => {
    if (snapshot) setDisplay(snapshot.display);
  }, [snapshot?.display.zoom, snapshot?.display.tool]);

  useEffect(() => {
    document.title = r.previewTitle;
  }, [r.previewTitle]);

  // The raster is read once per frame through the owner-scoped host command; no image data crosses windows.
  const token = snapshot?.owner.token ?? null;
  const frameId = snapshot?.frame?.id ?? null;
  useEffect(() => {
    if (snapshot === null || frameId === null) {
      setRaster(null);
      return;
    }
    let alive = true;
    let url: string | null = null;
    setRasterError(null);
    invoke<ArrayBuffer>('recognition_preview', {owner: snapshot.owner, frameId}).then(bytes => {
      if (!alive) return;
      url = URL.createObjectURL(new Blob([bytes], {type: 'image/png'}));
      setRaster({frameId, url});
    }).catch(cause => {if (alive) {setRaster(null); setRasterError(fault(cause));}});
    return () => {
      alive = false;
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [token, frameId]);

  useEffect(() => {
    const element = stage.current;
    if (!element) return;
    const observer = new ResizeObserver(() => setViewport({width: element.clientWidth, height: element.clientHeight}));
    observer.observe(element);
    return () => observer.disconnect();
  }, [Boolean(snapshot?.frame && snapshot.basis)]);

  function send(edit: PreviewEdit) {
    if (snapshot === null) return;
    const message: PreviewEditMessage = {token: snapshot.owner.token, frameId: snapshot.frame?.id ?? null, basisRevision: snapshot.basisRevision, edit};
    setSendError(null);
    emitTo(MAIN_LABEL, PREVIEW_EDIT, message).catch(cause => setSendError(fault(cause)));
  }

  function changeDisplay(next: PreviewDisplay) {
    setDisplay(next);
    send({kind: 'display', display: next});
  }

  const frame = snapshot?.frame ?? null;
  const basis = snapshot?.basis ?? null;
  const editable = snapshot?.editable ?? false;
  const scale = frame ? displayScale(display.zoom, frame.width, frame.height, viewport) : 1;
  const percent = Math.round(scale * 100);
  const selected = snapshot?.definitions.find(item => item.id === snapshot.selected) ?? null;

  function keyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!snapshot || !editable) return;
    const target = event.target as HTMLElement;
    if (target.closest('input, select, textarea')) return;
    if ((event.metaKey || event.ctrlKey) && !event.shiftKey && event.key.toLowerCase() === 'z') {
      event.preventDefault();
      if (snapshot.canUndo) send({kind: 'undo', localRevision: snapshot.localRevision});
    } else if ((event.key === 'Delete' || event.key === 'Backspace') && selected && display.tool === 'zones') {
      event.preventDefault();
      send({kind: 'delete', id: selected.id, revision: selected.revision});
    }
  }

  const body = !received
    ? <p className="preview-message muted">{r.previewWaiting}</p>
    : snapshot === null
      ? <p className="preview-message">{r.previewNoOwner}</p>
      : !frame || !basis
        ? <p className="preview-message">{r.previewNoFrame}</p>
        : <div ref={stage} className={display.zoom === 'fit' ? 'recognition-stage fit' : 'recognition-stage'}>
          <RecognitionCanvas width={frame.width} height={frame.height} basis={basis} definitions={snapshot.definitions} selected={snapshot.selected}
            observations={snapshot.observations} image={raster?.frameId === frame.id ? raster.url : null} scale={scale} tool={display.tool}
            editable={editable} generation={snapshot} labels={{surface: r.surfaceLabel, search: r.searchLabel, observed: r.observed}} onEdit={send}/>
        </div>;

  return <LocaleContext value={locale}>
    <div className="preview-app" onKeyDown={keyDown}>
      <header className="preview-toolbar">
        <div className="segmented" role="group" aria-label={r.toolLabel}>
          <button id="preview-tool-zones" type="button" aria-pressed={display.tool === 'zones'} disabled={!frame}
            onClick={() => changeDisplay({...display, tool: 'zones'})}>{r.toolZones}</button>
          <button id="preview-tool-content" type="button" aria-pressed={display.tool === 'content'} disabled={!frame}
            onClick={() => changeDisplay({...display, tool: 'content'})}>{r.toolContent}</button>
        </div>
        <div className="segmented" role="group" aria-label={r.zoomLabel}>
          <button id="preview-zoom-fit" type="button" aria-pressed={display.zoom === 'fit'} disabled={!frame}
            onClick={() => changeDisplay({...display, zoom: 'fit'})}>{r.fit}</button>
          <button id="preview-zoom-out" type="button" aria-label={r.zoomOut} title={r.zoomOut} disabled={!frame || percent <= ZOOM_LEVELS[0]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, -1)})}>−</button>
          <label className="visually-hidden" htmlFor="preview-zoom">{r.zoomLabel}</label>
          <select id="preview-zoom" value={display.zoom === 'fit' ? 'fit' : String(display.zoom)} disabled={!frame}
            onChange={event => changeDisplay({...display, zoom: event.target.value === 'fit' ? 'fit' : Number(event.target.value)})}>
            <option value="fit">{display.zoom === 'fit' ? `${r.fit} · ${r.percent(percent)}` : r.fit}</option>
            {ZOOM_LEVELS.map(level => <option key={level} value={String(level)}>{r.percent(level)}</option>)}
          </select>
          <button id="preview-zoom-in" type="button" aria-label={r.zoomIn} title={r.zoomIn} disabled={!frame || percent >= ZOOM_LEVELS[ZOOM_LEVELS.length - 1]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, 1)})}>+</button>
        </div>
        <div className="button-row">
          <button id="preview-undo" type="button" disabled={!editable || !snapshot?.canUndo}
            onClick={() => snapshot && send({kind: 'undo', localRevision: snapshot.localRevision})}>{r.undo}</button>
          <button id="preview-delete" type="button" className="danger-text" disabled={!editable || !selected}
            onClick={() => selected && send({kind: 'delete', id: selected.id, revision: selected.revision})}>{r.delete}</button>
        </div>
        {snapshot?.running && <button id="preview-stop" type="button" className="stop-button" onClick={() => {
          setStopError(null);
          // The independent authoring Stop: no geometry fence, no relay through the main window.
          invoke<boolean>('authoring_stop', {owner: snapshot.owner}).catch(cause => setStopError(fault(cause)));
        }}>{r.stop}</button>}
        {frame && <span className={snapshot?.confirmed ? 'tag current' : 'tag stale'}>{snapshot?.confirmed ? r.confirmed : r.unconfirmed}</span>}
        {frame && <span id="preview-scale" className="muted mono">{r.scale(frame.width, frame.height, percent)}</span>}
      </header>
      <p className="preview-help field-help">{display.tool === 'content' ? r.contentHelp : r.zonesHelp}</p>
      {stopError && <FaultMessage title={r.stopFailed} value={stopError}/>}
      {snapshot && !editable && <p className="inline-warning">{snapshot.lockReason ? `${r.readOnly} · ${snapshot.lockReason}` : r.readOnly}</p>}
      {snapshot?.notice && <p id="preview-notice" className="inline-warning" role="status">{r.notice(snapshot.notice)}</p>}
      {rasterError && <FaultMessage title={r.previewImageFailed} value={rasterError}/>}
      {sendError && <FaultMessage title={r.previewSendFailed} value={sendError}/>}
      {body}
    </div>
  </LocaleContext>;
}

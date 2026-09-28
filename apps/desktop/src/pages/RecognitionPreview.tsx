import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {emitTo, listen} from '@tauri-apps/api/event';
import RecognitionCanvas from '../components/RecognitionCanvas.tsx';
import NativeCaptureControls from '../components/NativeCaptureControls.tsx';
import Select from '../components/Select.tsx';
import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {LocaleContext} from '../locale.tsx';
import {PREVIEW_ACTION, PREVIEW_EDIT, PREVIEW_READY, PREVIEW_STATE, ZOOM_LEVELS, displayScale, stepZoom} from '../recognition.ts';
import type {PreviewAction, PreviewActionMessage, PreviewCloseFailure, PreviewDisplay, PreviewEdit, PreviewEditMessage, PreviewSnapshot, PreviewStateMessage} from '../recognition.ts';
import type {Fault} from '../types.ts';

// The main window's label in tauri.conf.json; it owns the Edit session and applies every relayed edit.
const MAIN_LABEL = 'main';

interface Raster {captureId: string; frameId: string; url: string}

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
  const [closeError, setCloseError] = useState<Fault | null>(null);
  const [dismissedNativeError, setDismissedNativeError] = useState<string | null>(null);
  const [closing, setClosing] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const helpButton = useRef<HTMLButtonElement>(null);
  const [display, setDisplay] = useState<PreviewDisplay>({zoom: 'fit', tool: 'zones'});
  const [viewport, setViewport] = useState({width: 0, height: 0});
  const stage = useRef<HTMLDivElement>(null);
  const publication = useRef(-1);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  const locale = snapshot?.locale ?? 'en';
  const r = messages[locale].ui.recognition;
  const native = messages[locale].ui.nativeCapture;

  useEffect(() => {
    let alive = true;
    let stop: (() => void) | null = null;
    void listen<PreviewStateMessage>(PREVIEW_STATE, event => {
      if (event.payload.publication <= publication.current) return;
      publication.current = event.payload.publication;
      setSnapshot(event.payload.snapshot);
      setReceived(true);
    }).then(unlisten => {
      if (!alive) {unlisten(); return;}
      stop = unlisten;
      return emitTo(MAIN_LABEL, PREVIEW_READY);
    }).catch(cause => setSendError(fault(cause)));
    return () => {alive = false; stop?.();};
  }, []);
  useEffect(() => {
    let alive = true;
    let stop: (() => void)|null = null;
    void listen<PreviewCloseFailure>('recognition-preview-close-failed', event => {
      const current = snapshotRef.current;
      if (!current || event.payload.owner.token !== current.owner.token || event.payload.revision !== current.revision
        || event.payload.generation !== (current.nativeSelection?.selection_generation ?? 0)) return;
      setCloseError(event.payload.error);
      setClosing(false);
    }).then(unlisten => {if (alive) stop = unlisten; else unlisten();}).catch(cause => setCloseError(fault(cause)));
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
  const captureId = snapshot?.capture_id ?? null;
  const nativeError = snapshot?.nativeSelection?.error ?? null;
  const nativeErrorKey = nativeError === null ? null
    : JSON.stringify([token, snapshot?.nativeSelection?.selection_generation, nativeError]);
  const nativeErrorHidden = nativeErrorKey !== null && dismissedNativeError === nativeErrorKey;
  const duplicateNativeError = nativeError !== null && snapshot?.error?.category === nativeError.category
    && snapshot.error.message === nativeError.message;
  useEffect(() => {
    if (nativeErrorKey === null || snapshot?.nativeSelection?.busy) setDismissedNativeError(null);
  }, [nativeErrorKey, snapshot?.nativeSelection?.busy]);
  useEffect(() => {
    if (snapshot === null || frameId === null || captureId === null) {
      setRaster(null);
      return;
    }
    let alive = true;
    let url: string | null = null;
    setRasterError(null);
    invoke<ArrayBuffer>('recognition_preview', {owner: snapshot.owner, frameId, captureId}).then(bytes => {
      if (!alive) return;
      url = URL.createObjectURL(new Blob([bytes], {type: 'image/png'}));
      setRaster({captureId, frameId, url});
    }).catch(cause => {if (alive) {setRaster(null); setRasterError(fault(cause));}});
    return () => {
      alive = false;
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [token, captureId, frameId]);

  useEffect(() => {
    const element = stage.current;
    if (!element) return;
    const observer = new ResizeObserver(() => setViewport({width: element.clientWidth, height: element.clientHeight}));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  function send(edit: PreviewEdit) {
    if (snapshot === null) return;
    const message: PreviewEditMessage = {token: snapshot.owner.token, capture_id: snapshot.capture_id, revision: snapshot.revision,
      frameId: snapshot.frame?.id ?? null, basisRevision: snapshot.basisRevision, edit};
    setSendError(null);
    emitTo(MAIN_LABEL, PREVIEW_EDIT, message).catch(cause => setSendError(fault(cause)));
  }

  function changeDisplay(next: PreviewDisplay) {
    setDisplay(next);
    send({kind: 'display', display: next});
  }
  function action(next:PreviewAction) {
    if (snapshot === null) return;
    const message:PreviewActionMessage = {token:snapshot.owner.token, revision:snapshot.revision,
      capture_id:snapshot.capture_id, documentRevision:snapshot.documentRevision, localRevision:snapshot.localRevision,
      generation:snapshot.nativeSelection?.selection_generation ?? 0, action:next};
    setSendError(null);
    setDismissedNativeError(null);
    emitTo(MAIN_LABEL, PREVIEW_ACTION, message).catch(cause => setSendError(fault(cause)));
  }

  async function done() {
    if (!snapshot || closing) return;
    setClosing(true);
    setCloseError(null);
    try {
      // Host proves the fenced Engine release and closes this window; no independent JS close can mask cleanup.
      await invoke('recognition_close_preview', {owner:snapshot.owner, revision:snapshot.revision,
        generation:snapshot.nativeSelection?.selection_generation ?? 0});
    } catch (cause) {
      setCloseError(fault(cause));
      setClosing(false);
    }
  }

  const frame = snapshot?.frame ?? null;
  const basis = snapshot?.basis ?? null;
  const editable = snapshot?.editable ?? false;
  const scale = frame ? displayScale(display.zoom, frame.width, frame.height, viewport) : 1;
  const percent = Math.round(scale * 100);
  const selected = snapshot?.definitions.find(item => item.id === snapshot.selected) ?? null;

  function keyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.defaultPrevented) return;
    const target = event.target as HTMLElement;
    if (target.closest('input, textarea, select, a, [contenteditable="true"], [role="combobox"], [role="listbox"], button:not(#preview-help-toggle)')) return;
    if (event.key === 'Escape' && helpOpen) {
      event.preventDefault();
      event.stopPropagation();
      setHelpOpen(false);
      helpButton.current?.focus();
      return;
    }
    if (target.closest('.preview-feedback, button') || !snapshot || !editable) return;
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
        : !snapshot.frameGeometryReady
          ? raster?.captureId === snapshot.capture_id && raster?.frameId === frame.id
            ? <img src={raster.url} width={frame.width} height={frame.height} alt=""/>
            : null
          : <RecognitionCanvas width={frame.width} height={frame.height} basis={basis} definitions={snapshot.definitions} selected={snapshot.selected}
            observations={snapshot.observations} image={raster?.captureId === snapshot.capture_id && raster?.frameId === frame.id ? raster.url : null} scale={scale} tool={display.tool}
            editable={editable} generation={snapshot} labels={{surface: r.surfaceLabel, search: r.searchLabel, observed: r.observed}} onEdit={send}/>;

  const hasFeedback = Boolean((nativeError && !nativeErrorHidden && !duplicateNativeError) || snapshot?.nativeCache?.error
    || (frame && snapshot && !snapshot.frameGeometryReady) || stopError || closeError
    || (snapshot?.error && (!duplicateNativeError || !nativeErrorHidden)) || (snapshot && !editable)
    || snapshot?.notice || rasterError || sendError);
  const status = snapshot?.nativeSelection?.busy ? native.status[snapshot.nativeSelection.status]
    : closing ? r.previewClosing : snapshot?.commandBusy ? r.previewBusy
      : snapshot?.running ? messages[locale].ui.phase('running')
        : frame ? snapshot?.confirmed ? r.confirmed : r.unconfirmed : '';

  return <LocaleContext value={locale}>
    <div className="preview-app" onKeyDown={keyDown}>
      <header className="preview-toolbar">
        <div className="segmented preview-tools" role="group" aria-label={r.toolLabel}>
          <button id="preview-tool-zones" type="button" aria-pressed={display.tool === 'zones'} disabled={!frame}
            onClick={() => changeDisplay({...display, tool: 'zones'})}>{r.toolZones}</button>
          <button id="preview-tool-content" type="button" aria-pressed={display.tool === 'content'} disabled={!frame || !editable || !snapshot?.frameGeometryReady}
            onClick={() => changeDisplay({...display, tool: 'content'})}>{r.toolContent}</button>
        </div>
        <div className="segmented preview-zoom-controls" role="group" aria-label={r.zoomLabel}>
          <button id="preview-zoom-fit" type="button" aria-pressed={display.zoom === 'fit'} disabled={!frame}
            onClick={() => changeDisplay({...display, zoom: 'fit'})}>{r.fit}</button>
          <button id="preview-zoom-out" type="button" aria-label={r.zoomOut} title={r.zoomOut} disabled={!frame || percent <= ZOOM_LEVELS[0]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, -1)})}>−</button>
          <Select id="preview-zoom" className="preview-zoom-select" aria-label={r.zoomLabel} value={display.zoom === 'fit' ? 'fit' : String(display.zoom)} disabled={!frame}
            onChange={value => changeDisplay({...display, zoom: value === 'fit' ? 'fit' : Number(value)})}
            options={[{value: 'fit', label: display.zoom === 'fit' ? `${r.fit} · ${r.percent(percent)}` : r.fit},
              ...ZOOM_LEVELS.map(level => ({value: String(level), label: r.percent(level)}))]}/>
          <button id="preview-zoom-in" type="button" aria-label={r.zoomIn} title={r.zoomIn} disabled={!frame || percent >= ZOOM_LEVELS[ZOOM_LEVELS.length - 1]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, 1)})}>+</button>
        </div>
        <div className="button-row preview-edit-actions">
          <button id="preview-undo" type="button" disabled={!editable || !snapshot?.canUndo}
            onClick={() => snapshot && send({kind: 'undo', localRevision: snapshot.localRevision})}>{r.undo}</button>
          <button id="preview-delete" type="button" className="danger-text" disabled={!editable || !selected}
            onClick={() => selected && send({kind: 'delete', id: selected.id, revision: selected.revision})}>{r.delete}</button>
        </div>
        <div className="preview-actions">
          <button id="preview-stop" type="button" className="stop-button" disabled={!snapshot?.running}
            style={{visibility: snapshot?.running ? 'visible' : 'hidden'}} onClick={() => {
              if (!snapshot) return;
              setStopError(null);
              // The independent authoring Stop: no geometry fence, no relay through the main window.
              invoke<boolean>('authoring_stop', {owner: snapshot.owner}).catch(cause => setStopError(fault(cause)));
            }}>{r.stop}</button>
          <NativeCaptureControls disabled={!snapshot || closing || snapshot.commandBusy || snapshot.running || !snapshot.editable} cancelDisabled={closing}
            selection={snapshot?.nativeSelection ?? null} onSelect={() => action('select')} onStart={() => action('start')}
            onCapture={newCapture => action(newCapture ? 'newCapture' : 'capture')} onCancel={() => action('cancel')}/>
          <button id="preview-done" type="button" disabled={!snapshot || closing}
            onClick={() => void done()}>{r.previewDone}</button>
        </div>
      </header>
      <main className="preview-viewport">
        <div ref={stage} id="preview-image-viewport"
          className={`recognition-stage${frame && !snapshot?.frameGeometryReady ? ' preview-raw' : display.zoom === 'fit' ? ' fit' : ''}`}>
          {body}
        </div>
        {(helpOpen || hasFeedback) && <aside id="preview-feedback" className="preview-feedback" aria-label={r.previewFeedback} tabIndex={0}>
          {helpOpen && <section id="preview-help" aria-labelledby="preview-help-toggle" tabIndex={0}>
            <p className="field-help">{display.tool === 'content' ? r.contentHelp : r.zonesHelp}</p>
          </section>}
          {nativeError && !nativeErrorHidden && !duplicateNativeError
            && <FaultMessage title={native.status.failed} value={nativeError} onDismiss={() => setDismissedNativeError(nativeErrorKey)}/>}
          {snapshot?.nativeCache?.error && <FaultMessage title={native.cacheFailed} value={snapshot.nativeCache.error}/>}
          {frame && snapshot && !snapshot.frameGeometryReady && <p className="inline-warning" role="status">
            {r.newFrameGeometry}
            <button id="preview-rebase" type="button" disabled={!editable || snapshot.commandBusy} onClick={() => send({kind:'rebase'})}>{r.rebaseFrame}</button>
          </p>}
          {stopError && <FaultMessage title={r.stopFailed} value={stopError}/>}
          {closeError && <FaultMessage title={r.previewCloseFailed} value={closeError}/>}
          {snapshot?.error && (!duplicateNativeError || !nativeErrorHidden)
            && <FaultMessage title={r.actionFailed} value={snapshot.error}
              onDismiss={duplicateNativeError ? () => setDismissedNativeError(nativeErrorKey) : undefined}/>}
          {snapshot && !editable && <p className="inline-warning">{snapshot.lockReason ? `${r.readOnly} · ${snapshot.lockReason}` : r.readOnly}</p>}
          {snapshot?.notice && <p id="preview-notice" className="inline-warning" role="status">{r.notice(snapshot.notice)}</p>}
          {rasterError && <FaultMessage title={r.previewImageFailed} value={rasterError}/>}
          {sendError && <FaultMessage title={r.previewSendFailed} value={sendError}/>}
        </aside>}
      </main>
      <footer className="preview-status-rail">
        <span id="native-capture-status" className="preview-capture-status muted" role="status" title={status}>{status}</span>
        <span id="preview-scale" className="muted mono" title={frame ? r.scale(frame.width, frame.height, percent) : undefined}>
          {frame && r.scale(frame.width, frame.height, percent)}
        </span>
        <button ref={helpButton} id="preview-help-toggle" type="button" aria-expanded={helpOpen} aria-controls="preview-help"
          onClick={() => setHelpOpen(value => !value)}>{r.previewHelpButton}</button>
      </footer>
    </div>
  </LocaleContext>;
}

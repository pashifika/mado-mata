import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {emitTo, listen, TauriEvent} from '@tauri-apps/api/event';
import RecognitionCanvas from '../components/RecognitionCanvas.tsx';
import ContentTrial from '../components/ContentTrial.tsx';
import NativeCaptureControls from '../components/NativeCaptureControls.tsx';
import Select from '../components/Select.tsx';
import {FaultMessage, fault} from '../components/ResultPanel.tsx';
import {messages} from '../i18n.ts';
import {LocaleContext} from '../locale.tsx';
import {PREVIEW_ACTION, PREVIEW_EDIT, PREVIEW_LABEL, PREVIEW_READY, PREVIEW_STATE, ZOOM_LEVELS, displayScale, sameJson, stepZoom} from '../recognition.ts';
import type {PixelRect, PreviewAction, PreviewActionMessage, PreviewCloseFailure, PreviewDisplay, PreviewEdit, PreviewEditMessage, PreviewSnapshot, PreviewStateMessage} from '../recognition.ts';
import type {Fault} from '../types.ts';
import {PixelInspection, rgbHex} from '../recognitionInspection.ts';
import type {InspectionState, PixelSample} from '../recognitionInspection.ts';

// The main window's label in tauri.conf.json; it owns the Edit session and applies every relayed edit.
const MAIN_LABEL = 'main';

interface Raster {epoch: number; token: string; captureId: string; frameId: string; frameRevision: number; url: string}

function contentIdentity(snapshot: PreviewSnapshot | null): string {
  return JSON.stringify(snapshot && [snapshot.owner.token, snapshot.revision, snapshot.capture_id, snapshot.frame?.id,
    snapshot.frame?.revision, snapshot.frame?.width, snapshot.frame?.height, snapshot.basisRevision,
    snapshot.editable, snapshot.lockReason, snapshot.commandBusy, snapshot.running, snapshot.frameGeometryReady,
    snapshot.nativeSelection?.busy, snapshot.display.tool]);
}

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
  const [, setInspectionState] = useState<InspectionState | null>(null);
  const [inspection] = useState(() => new PixelInspection(
    request => invoke<PixelSample>('recognition_pixel', {...request}), setInspectionState, fault));
  const inspected = inspection.value;
  const [closing, setClosing] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [trialEpoch, setTrialEpoch] = useState(0);
  const trialEpochRef = useRef(0);
  const helpButton = useRef<HTMLButtonElement>(null);
  const [display, setDisplay] = useState<PreviewDisplay>({zoom: 'fit', tool: 'zones'});
  const [viewport, setViewport] = useState({width: 0, height: 0});
  const stage = useRef<HTMLDivElement>(null);
  const publication = useRef(-1);
  const snapshotRef = useRef(snapshot);
  const inspectionEpoch = inspection.sourceEpoch;
  const locale = snapshot?.locale ?? 'en';
  const r = messages[locale].ui.recognition;
  const native = messages[locale].ui.nativeCapture;

  function invalidateContent() { setTrialEpoch(++trialEpochRef.current); }

  useEffect(() => {
    inspection.open();
    let alive = true;
    let stop: (() => void) | null = null;
    void listen<PreviewStateMessage>(PREVIEW_STATE, event => {
      if (!alive || event.payload.publication <= publication.current) return;
      publication.current = event.payload.publication;
      // Count every identity transition, even if React batches A → B → A into one render.
      if (contentIdentity(snapshotRef.current) !== contentIdentity(event.payload.snapshot)) invalidateContent();
      snapshotRef.current = event.payload.snapshot;
      setDisplay(inspection.incoming(event.payload.snapshot));
      setSnapshot(event.payload.snapshot);
      setReceived(true);
    }).then(unlisten => {
      if (!alive) {unlisten(); return;}
      stop = unlisten;
      return emitTo(MAIN_LABEL, PREVIEW_READY);
    }).catch(cause => setSendError(fault(cause)));
    return () => {alive = false; stop?.(); inspection.close();};
  }, []);
  useEffect(() => {
    let alive = true;
    const stops: (() => void)[] = [];
    const register = (promise: Promise<() => void>) => {
      void promise.then(stop => {if (alive) stops.push(stop); else stop();})
        .catch(cause => {if (alive) setCloseError(fault(cause));});
    };
    register(listen(TauriEvent.WINDOW_CLOSE_REQUESTED, () => {
      if (!alive) return;
      inspection.close();
      invalidateContent();
      setClosing(true);
    }, {target:{kind:'Window', label:PREVIEW_LABEL}}));
    register(listen<PreviewCloseFailure>('recognition-preview-close-failed', event => {
      if (!alive) return;
      const current = snapshotRef.current;
      if (!current || !sameJson(event.payload.owner, current.owner) || event.payload.revision !== current.revision
        || event.payload.generation !== (current.nativeSelection?.selection_generation ?? 0)) return;
      setCloseError(event.payload.error);
      inspection.open();
      setClosing(false);
    }));
    return () => {alive = false; for (const stop of stops) stop();};
  }, []);


  useEffect(() => {
    document.title = r.previewTitle;
  }, [r.previewTitle]);

  // The raster is read once per frame through the owner-scoped host command; no image data crosses windows.
  const token = snapshot?.owner.token ?? null;
  const frameId = snapshot?.frame?.id ?? null;
  const frameRevision = snapshot?.frame?.revision ?? null;
  const captureId = snapshot?.capture_id ?? null;
  const image = raster?.epoch === inspectionEpoch && raster?.token === token && raster?.captureId === captureId && raster?.frameId === frameId
    && raster?.frameRevision === frameRevision ? raster.url : null;
  const trialKey = JSON.stringify([trialEpoch, contentIdentity(snapshot), closing, display.tool, image]);
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
    if (closing || snapshot === null || frameId === null || captureId === null) {
      setRaster(null);
      inspection.setRaster(inspectionEpoch, null);
      return;
    }
    let alive = true;
    let url: string | null = null;
    setRasterError(null);
    invoke<ArrayBuffer>('recognition_preview', {owner: snapshot.owner, frameId, captureId}).then(bytes => {
      if (!alive || inspection.sourceEpoch !== inspectionEpoch) return;
      url = URL.createObjectURL(new Blob([bytes], {type: 'image/png'}));
      inspection.setRaster(inspectionEpoch, url);
      setRaster({epoch:inspectionEpoch, token: snapshot.owner.token, captureId, frameId, frameRevision: snapshot.frame!.revision, url});
    }).catch(cause => {
      if (alive && inspection.sourceEpoch === inspectionEpoch) {
        inspection.setRaster(inspectionEpoch, null);
        setRaster(null);
        setRasterError(fault(cause));
      }
    });
    return () => {
      alive = false;
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [token, captureId, frameId, frameRevision, inspectionEpoch, closing]);

  useEffect(() => {
    const element = stage.current;
    if (!element) return;
    const measure = () => setViewport({width: element.clientWidth, height: element.clientHeight});
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  function send(edit: PreviewEdit) {
    if (snapshot === null || closing || ((display.tool === 'inspect' || inspection.inspecting) && edit.kind !== 'display')) return;
    const message: PreviewEditMessage = {token: snapshot.owner.token, capture_id: snapshot.capture_id, revision: snapshot.revision,
      frameId: snapshot.frame?.id ?? null, basisRevision: snapshot.basisRevision, edit};
    setSendError(null);
    emitTo(MAIN_LABEL, PREVIEW_EDIT, message).catch(cause => setSendError(fault(cause)));
  }

  function changeDisplay(next: PreviewDisplay) {
    if (next.tool !== display.tool) invalidateContent();
    inspection.setDisplay(next);
    setDisplay(next);
    send({kind: 'display', display: next});
  }
  function action(next:PreviewAction) {
    if (snapshot === null) return;
    invalidateContent();
    inspection.clear();
    const message:PreviewActionMessage = {token:snapshot.owner.token, revision:snapshot.revision,
      capture_id:snapshot.capture_id, documentRevision:snapshot.documentRevision, localRevision:snapshot.localRevision,
      generation:snapshot.nativeSelection?.selection_generation ?? 0, action:next};
    setSendError(null);
    setDismissedNativeError(null);
    emitTo(MAIN_LABEL, PREVIEW_ACTION, message).catch(cause => setSendError(fault(cause)));
  }

  async function done() {
    if (!snapshot || closing) return;
    invalidateContent();
    inspection.close();
    setClosing(true);
    setCloseError(null);
    try {
      // Host proves the fenced Engine release and closes this window; no independent JS close can mask cleanup.
      await invoke('recognition_close_preview', {owner:snapshot.owner, revision:snapshot.revision,
        generation:snapshot.nativeSelection?.selection_generation ?? 0});
    } catch (cause) {
      setCloseError(fault(cause));
      inspection.open();
      setClosing(false);
    }
  }

  const frame = snapshot?.frame ?? null;
  const basis = snapshot?.basis ?? null;
  const editable = Boolean(snapshot?.editable && !snapshot.commandBusy && !snapshot.running && !closing);
  const geometryEditable = editable && display.tool !== 'inspect';
  const trialDisabled = !geometryEditable || !snapshot?.frameGeometryReady || Boolean(snapshot?.lockReason || snapshot?.nativeSelection?.busy);
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
    if (target.closest('.preview-feedback, .content-trial, button') || !snapshot || !geometryEditable) return;
    if ((event.metaKey || event.ctrlKey) && !event.shiftKey && event.key.toLowerCase() === 'z') {
      event.preventDefault();
      if (snapshot.canUndo) send({kind: 'undo', localRevision: snapshot.localRevision});
    } else if ((event.key === 'Delete' || event.key === 'Backspace') && selected && display.tool === 'zones') {
      event.preventDefault();
      send({kind: 'delete', id: selected.id, revision: selected.revision});
    }
  }

  // Keep the same image element in every tool, including raw frames awaiting geometry confirmation.
  const body = (candidate: PixelRect | null, clearCandidate: () => void) => !received
    ? <p className="preview-message muted">{r.previewWaiting}</p>
    : snapshot === null
      ? <p className="preview-message">{r.previewNoOwner}</p>
      : !frame
        ? <p className="preview-message">{r.previewNoFrame}</p>
        : <RecognitionCanvas width={frame.width} height={frame.height} basis={snapshot.frameGeometryReady ? basis : null}
            definitions={snapshot.definitions} selected={snapshot.selected} observations={snapshot.observations}
            image={image} scale={scale} tool={display.tool} editable={geometryEditable} generation={snapshot}
            labels={{surface:display.tool === 'inspect' ? r.inspectSurface : r.surfaceLabel, search:r.searchLabel, observed:r.observed}}
            inspection={inspection} inspectPoint={inspected.point}
            onImageReady={loaded => inspection.rasterLoaded(inspectionEpoch, loaded)}
            onImageError={failed => {
              if (!inspection.rasterFailed(inspectionEpoch, failed)) return;
              setRaster(null);
              setRasterError({category:'Image', message:r.previewImageFailed, context:null});
            }}
            contentCandidate={candidate} onContentEditStart={clearCandidate}
            onEdit={edit => {if (edit.kind === 'content') clearCandidate(); send(edit);}}/>;

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
      <ContentTrial identity={trialKey} active={display.tool === 'content'} disabled={trialDisabled}
        width={frame?.width ?? 0} height={frame?.height ?? 0} content={basis?.content ?? null} image={image}
        getImage={() => stage.current?.querySelector<HTMLImageElement>('.recognition-surface img') ?? null}
        isCurrent={() => trialEpochRef.current === trialEpoch && contentIdentity(snapshotRef.current) === contentIdentity(snapshot)}
        onApply={content => {
          if (trialEpochRef.current === trialEpoch && contentIdentity(snapshotRef.current) === contentIdentity(snapshot)) send({kind: 'content', content});
        }}>
      {(candidate, clearCandidate, controls, feedback) => <>
      <header className="preview-toolbar">
        <div className="segmented preview-tools" role="group" aria-label={r.toolLabel}>
          <button id="preview-tool-zones" type="button" aria-pressed={display.tool === 'zones'} disabled={!frame}
            onClick={() => changeDisplay({...display, tool: 'zones'})}>{r.toolZones}</button>
          <button id="preview-tool-content" type="button" aria-pressed={display.tool === 'content'} disabled={!frame || !editable || !snapshot?.frameGeometryReady}
            onClick={() => changeDisplay({...display, tool: 'content'})}>{r.toolContent}</button>
          <button id="preview-tool-inspect" type="button" aria-pressed={display.tool === 'inspect'} disabled={!frame || closing}
            onClick={() => changeDisplay({...display, tool:'inspect'})}>{r.toolInspect}</button>
        </div>
        <div className="segmented preview-zoom-controls" role="group" aria-label={r.zoomLabel}>
          <button id="preview-zoom-fit" type="button" aria-pressed={display.zoom === 'fit'} disabled={!frame}
            onClick={() => changeDisplay({...display, zoom: 'fit'})}>{r.fit}</button>
          <button id="preview-zoom-out" type="button" aria-label={r.zoomOut} title={r.zoomOut} disabled={!frame || percent <= ZOOM_LEVELS[0]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, -1)})}>−</button>
          <Select id="preview-zoom" className="preview-zoom-select" aria-label={r.zoomLabel} value={display.zoom === 'fit' ? 'fit' : String(display.zoom)} disabled={!frame}
            onChange={value => changeDisplay({...display, zoom: value === 'fit' ? 'fit' : Number(value)})}
            options={[{value: 'fit', label: display.zoom === 'fit' ? r.percent(percent) : r.fit},
              ...ZOOM_LEVELS.map(level => ({value: String(level), label: r.percent(level)}))]}/>
          <button id="preview-zoom-in" type="button" aria-label={r.zoomIn} title={r.zoomIn} disabled={!frame || percent >= ZOOM_LEVELS[ZOOM_LEVELS.length - 1]}
            onClick={() => changeDisplay({...display, zoom: stepZoom(scale, 1)})}>+</button>
        </div>
        {display.tool === 'content' ? controls : <div className="button-row preview-edit-actions">
          <button id="preview-undo" type="button" disabled={!geometryEditable || !snapshot?.canUndo}
            onClick={() => snapshot && send({kind: 'undo', localRevision: snapshot.localRevision})}>{r.undo}</button>
          <button id="preview-delete" type="button" className="danger-text" disabled={!geometryEditable || !selected}
            onClick={() => selected && send({kind: 'delete', id: selected.id, revision: selected.revision})}>{r.delete}</button>
        </div>}
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
          className={`recognition-stage${display.zoom === 'fit' ? ' fit' : ''}`}>
          {body(candidate, clearCandidate)}
        </div>
        {(helpOpen || hasFeedback) && <aside id="preview-feedback" className="preview-feedback" aria-label={r.previewFeedback} tabIndex={0}>
          {helpOpen && <section id="preview-help" aria-labelledby="preview-help-toggle" tabIndex={0}>
            <p className="field-help">{display.tool === 'inspect' ? r.inspectHelp : display.tool === 'content' ? r.contentHelp : r.zonesHelp}</p>
          </section>}
          {nativeError && !nativeErrorHidden && !duplicateNativeError
            && <FaultMessage title={native.status.failed} value={nativeError} onDismiss={() => setDismissedNativeError(nativeErrorKey)}/>}
          {snapshot?.nativeCache?.error && <FaultMessage title={native.cacheFailed} value={snapshot.nativeCache.error}/>}
          {frame && snapshot && !snapshot.frameGeometryReady && <p className="inline-warning" role="status">
            {r.newFrameGeometry}
            <button id="preview-rebase" type="button" disabled={!geometryEditable || snapshot.commandBusy} onClick={() => send({kind:'rebase'})}>{r.rebaseFrame}</button>
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
        {display.tool === 'inspect' ? <section className="preview-inspection" aria-label={r.inspectTitle} title={r.inspectTitle}>
          <dl className="inspection-values mono">
            <div><dt>X</dt><dd id="inspect-x">{inspected.point?.x ?? '—'}</dd></div>
            <div><dt>Y</dt><dd id="inspect-y">{inspected.point?.y ?? '—'}</dd></div>
            {(['R', 'G', 'B', 'A'] as const).map((channel, index) => <div key={channel}>
              <dt>{channel}</dt><dd id={`inspect-${channel.toLowerCase()}`}>{inspected.sample?.rgba[index] ?? '—'}</dd>
            </div>)}
          </dl>
          <div className="inspection-color-line">
            <span className="inspection-swatch" role="img" aria-label={r.inspectSwatch}>
              {inspected.sample && <span style={{backgroundColor:`rgba(${inspected.sample.rgba.slice(0, 3).join(',')},${inspected.sample.rgba[3] / 255})`}}/>}
            </span>
            <span className="inspection-hex mono">{r.inspectHex} <span id="inspect-hex">{inspected.sample ? rgbHex(inspected.sample.rgba) : '—'}</span></span>
            <span id="inspect-status" className={`inspection-status${inspected.error ? ' danger-text' : ' muted'}`} role="status"
              title={inspected.error ? `${inspected.error.category}: ${inspected.error.message}` : undefined}>
              {inspected.pending ? r.inspectPending : inspected.error
                ? `${r.inspectFailed} · ${inspected.error.category}: ${inspected.error.message}` : inspected.point ? '' : r.inspectEmpty}
            </span>
          </div>
        </section> : <>
          <span id="native-capture-status" className="preview-capture-status muted" role="status" title={status}>{status}</span>
          {feedback}
          <span id="preview-scale" className="muted mono" title={frame ? r.scale(frame.width, frame.height, percent) : undefined}>
            {frame && r.scale(frame.width, frame.height, percent)}
          </span>
        </>}
        <button ref={helpButton} id="preview-help-toggle" type="button" aria-expanded={helpOpen} aria-controls="preview-help"
          onClick={() => setHelpOpen(value => !value)}>{r.previewHelpButton}</button>
      </footer>
      </>}
      </ContentTrial>
    </div>
  </LocaleContext>;
}

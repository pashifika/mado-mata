import Modal from './Modal.tsx';
import {PublicationResult, ReviewFacts} from './ReleaseReview.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import {shortRevision} from '../authoring.ts';
import type {FileDraft} from '../authoring.ts';
import {shortDigest} from '../distribution.ts';
import type {ExportFlow, SignerState} from '../distribution.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  flow: ExportFlow | null;
  // The reviewed capture no longer matches the current Edit owner, saved revision or contributing drafts.
  stale: boolean;
  packageId: string; release: string | null;
  drafts: FileDraft[]; recognitionDirty: boolean;
  // Why the drafts cannot be saved as they are (a lost lease, a recognition refusal); Cancel stays available.
  saveBlock: string | null;
  signer: SignerState;
  // Why preparation or confirmation cannot start now: another command, an active operation, a changed owner, closing.
  blocked: string | null;
  onSave: () => void; onChoose: () => void; onCancelPreparation: () => void; onConfirm: () => void;
  onPrepareAgain: () => void; onSigning: () => void; onClose: () => void;
}

// Export PKG for the Edit owner: explicit Save and continue or Cancel for unsaved drafts, one destination choice, an
// owner-bound review of the captured saved revision and a publication receipt reported as the host committed it.
export default function ExportDialog({flow, stale, packageId, release, drafts, recognitionDirty, saveBlock, signer, blocked,
  onSave, onChoose, onCancelPreparation, onConfirm, onPrepareAgain, onSigning, onClose}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const d = ui.distribution;
  const a = ui.authoring;
  const step = flow?.step ?? null;
  const busy = step?.kind === 'saving' || step?.kind === 'choosing' || step?.kind === 'preparing' || step?.kind === 'publishing';
  const unsavable = drafts.some(draft => draft.missing);
  const signerUsable = signer.info != null && signer.statusError === null;
  const status = step?.kind === 'saving' ? d.saving : step?.kind === 'choosing' ? d.choosing : step?.kind === 'publishing' ? d.publishing
    : step?.kind === 'preparing' ? step.cancelling ? d.cancelling : '' : blocked ?? '';
  return <Modal id="export-dialog" open={flow !== null} onCancel={onClose} labelledBy="export-heading" locked={busy}
    initialFocus={step?.kind === 'drafts' ? '#export-cancel' : undefined}
    returnFocus={() => document.getElementById('authoring-export')}>
    <div className="dialog-header"><div><h2 id="export-heading">{d.exportHeading}</h2><p>{d.exportIntro}</p></div></div>
    {step && <div className="dialog-body">
      <dl className="run-identity">
        <dt>{d.packageId}</dt><dd id="export-package" className="mono">{packageId}</dd>
        <dt>{d.release}</dt><dd id="export-release" className="mono">{release ?? d.releaseUndeclared}</dd>
      </dl>
      {step.kind === 'drafts' && <>
        <p>{d.draftsHelp}</p>
        <ul id="export-drafts" className="saved-list">{drafts.map(draft => <li key={draft.path} className="saved-row">
          <span className="saved-text"><strong className="mono">{draft.path}</strong><span className="mono">{a.kind(draft.kind)}{draft.missing ? ` · ${a.missingHeading}` : ''}</span></span></li>)}
          {recognitionDirty && <li className="saved-row"><span className="saved-text"><strong>{a.recognitionDraft}</strong></span></li>}</ul>
        {unsavable && <p className="inline-warning">{a.dirtyUnsavable}</p>}
        {saveBlock && <p className="inline-warning">{saveBlock}</p>}
      </>}
      {step.kind === 'stopped' && <section id="export-stopped" aria-labelledby="export-stopped-heading">
        <h3 id="export-stopped-heading">{d.stoppedHeading}</h3><p className="muted">{d.stoppedHelp}</p>
        {step.sequence.committed.length > 0 && <dl className="run-identity"><dt>{d.committed}</dt><dd><ul className="distribution-names">
          {step.sequence.committed.map(item => <li key={`${item.path}:${item.revision}`}><span className="mono">{item.path}</span> · <span className="mono">{shortRevision(item.revision)}</span></li>)}</ul></dd></dl>}
        {step.remaining && <p className="inline-warning">{d.remainingDrafts}</p>}
        {step.sequence.failure?.error && <FaultMessage title={d.stoppedHeading} value={step.sequence.failure.error}/>}
      </section>}
      {(step.kind === 'ready' || step.kind === 'choosing') && <>
        <dl className="run-identity"><dt>{d.signer}</dt><dd id="export-signer" className={signerUsable ? 'mono' : undefined}>
          {signer.info ? shortDigest(signer.info.key_id) : signer.statusError ? d.statusFailed : signer.info === null ? d.noKey : d.checking}</dd></dl>
        {signer.info === null && signer.statusError === null && <p className="inline-warning">{d.signerMissing}</p>}
        {signer.statusError && <FaultMessage title={d.statusFailed} value={signer.statusError}/>}
        <div className="button-row"><button id="export-signing" type="button" aria-haspopup="dialog" disabled={step.kind === 'choosing'} onClick={onSigning}>{d.signingDetails}</button></div>
        {step.kind === 'ready' && step.note?.kind === 'unchosen' && <p id="export-unchosen" className="muted" role="status">{d.destinationCancelled}</p>}
        {step.kind === 'ready' && step.note?.kind === 'failed' && <FaultMessage title={step.note.stage === 'publish' ? d.publishFailed : d.prepareFailed} value={step.note.error}/>}
      </>}
      {step.kind === 'preparing' && <>
        <p id="export-progress" role="status">{d.preparing(shortRevision(step.ticket.revision))}</p>
        <dl className="run-identity"><dt>{d.destination}</dt><dd className="mono">{step.destination}</dd></dl>
        {step.cancelError && <FaultMessage title={d.cancelPreparation} value={step.cancelError}/>}
      </>}
      {(step.kind === 'review' || step.kind === 'publishing') && <>
        <h3>{d.reviewHeading}</h3>
        <ReviewFacts review={step.review} idPrefix="export" installing={false}/>
        {stale && step.kind === 'review' && <p id="export-stale" className="inline-warning" role="alert">{d.staleReview}</p>}
      </>}
      {step.kind === 'published' && <PublicationResult review={step.review} publication={step.publication} idPrefix="export" installing={false}/>}
    </div>}
    <div className="dialog-footer">
      <span id="export-status" role="status">{status}</span>
      {step?.kind === 'drafts' && <>
        <button id="export-cancel" type="button" onClick={onClose}>{ui.common.cancel}</button>
        <button id="export-save" type="button" className="primary" disabled={unsavable || saveBlock !== null || blocked !== null} onClick={onSave}>{d.saveContinue}</button>
      </>}
      {step?.kind === 'ready' && <>
        <button id="export-cancel" type="button" onClick={onClose}>{ui.common.cancel}</button>
        <button id="export-choose" type="button" className="primary" disabled={!signerUsable || blocked !== null} onClick={onChoose}>{d.chooseDestination}</button>
      </>}
      {step?.kind === 'preparing' && <button id="export-cancel-preparation" type="button" disabled={step.cancelling} onClick={onCancelPreparation}>{d.cancelPreparation}</button>}
      {step?.kind === 'review' && <>
        <button id="export-cancel" type="button" onClick={onClose}>{ui.common.cancel}</button>
        {stale
          ? <button id="export-prepare-again" type="button" className="primary" onClick={onPrepareAgain}>{d.prepareAgain}</button>
          : <button id="export-confirm" type="button" className="primary" disabled={blocked !== null} onClick={onConfirm}>{d.confirmExport}</button>}
      </>}
      {(step?.kind === 'stopped' || step?.kind === 'published') && <button id="export-close" type="button" className="primary" onClick={onClose}>{ui.common.close}</button>}
    </div>
  </Modal>;
}

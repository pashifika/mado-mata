import {ReactNode, useEffect} from 'react';
import Modal from './Modal.tsx';
import {PublicationResult, ReviewFacts} from './ReleaseReview.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import type {ImportFlow} from '../distribution.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  flow: ImportFlow | null; label: string;
  // The originating Tab session closed or moved to another revision after verification.
  stale: boolean;
  // Why confirmation cannot start now (another command, closing).
  blocked: string | null;
  strip: ReactNode;
  onConfirm: () => void; onCancelPreparation: () => void; onClose: () => void;
}

// Import PKG for one Tab: verification of one picked archive, review of that captured artifact and an explicit Install.
// Nothing here selects the release, changes another Tab or runs package code.
export default function ImportDialog({flow, label, stale, blocked, strip, onConfirm, onCancelPreparation, onClose}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const d = ui.distribution;
  const step = flow?.step ?? null;
  const busy = step?.kind === 'choosing' || step?.kind === 'preparing' || step?.kind === 'publishing';
  const status = step?.kind === 'publishing' ? d.installing : step?.kind === 'preparing' && step.cancelling ? d.cancelling : step?.kind === 'review' ? blocked ?? '' : '';
  // Recover unmounted actions to the footer (Cancel first), never to another operation's Stop.
  useEffect(() => {
    const dialog = document.getElementById('import-dialog');
    if (!(dialog instanceof HTMLDialogElement) || !dialog.open) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body && active !== dialog) return;
    (dialog.querySelector<HTMLElement>('.dialog-footer button:not(:disabled)') ?? dialog).focus();
  }, [step?.kind]);
  return <Modal id="import-dialog" open={flow !== null && step?.kind !== 'choosing'} onCancel={onClose} labelledBy="import-heading" locked={busy}
    initialFocus=".dialog-footer button:not(:disabled)" returnFocus={() => document.getElementById('installed-import')}>
    <div className="dialog-header"><div><span className="eyebrow">{label}</span><h2 id="import-heading">{d.importHeading}</h2><p>{d.importIntro}</p></div></div>
    {strip}
    {step && <div className="dialog-body">
      {step.kind === 'preparing' && <>
        <p id="import-progress" role="status">{d.verifying(step.path)}</p>
        {step.cancelError && <FaultMessage title={d.cancelPreparation} value={step.cancelError}/>}
      </>}
      {(step.kind === 'review' || step.kind === 'publishing') && <>
        <h3>{d.importReviewHeading}</h3>
        <ReviewFacts review={step.review} idPrefix="import" installing={true}/>
        {stale && step.kind === 'review' && <p id="import-stale" className="inline-warning" role="alert">{d.staleImport}</p>}
      </>}
      {step.kind === 'published' && <PublicationResult review={step.review} publication={step.publication} idPrefix="import" installing={true}/>}
      {step.kind === 'failed' && <FaultMessage title={d.importFailed} value={step.error}/>}
    </div>}
    <div className="dialog-footer">
      <span id="import-status" role="status">{status}</span>
      {step?.kind === 'preparing' && <button id="import-cancel-preparation" type="button" disabled={step.cancelling} onClick={onCancelPreparation}>{d.cancelPreparation}</button>}
      {step?.kind === 'review' && <>
        <button id="import-cancel" type="button" onClick={onClose}>{ui.common.cancel}</button>
        <button id="import-confirm" type="button" className="primary" disabled={stale || blocked !== null} onClick={onConfirm}>{d.confirmImport}</button>
      </>}
      {(step?.kind === 'published' || step?.kind === 'failed') && <button id="import-close" type="button" className="primary" onClick={onClose}>{ui.common.close}</button>}
    </div>
  </Modal>;
}

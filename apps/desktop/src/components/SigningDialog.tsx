import Modal from './Modal.tsx';
import {FaultMessage} from './ResultPanel.tsx';
import type {SignerState} from '../distribution.ts';
import {messages} from '../i18n.ts';
import type {Locale} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

interface Props {
  open: boolean; signer: SignerState;
  // Why enrollment cannot start now (for example the application is closing).
  enrollBlock: string | null;
  onEnroll: () => void; onCancelEnroll: () => void; onClose: () => void;
}

const enrolledFormats: Record<Locale, Intl.DateTimeFormat> = {
  en: new Intl.DateTimeFormat('en', {dateStyle: 'medium', timeStyle: 'short'}),
  ja: new Intl.DateTimeFormat('ja', {dateStyle: 'medium', timeStyle: 'short'}),
};

// Public facts of the local signing key and the one real enrollment action. The account label is a local association,
// never a verified publisher identity; enrollment is only needed to export, never to edit, import or run.
export default function SigningDialog({open, signer, enrollBlock, onEnroll, onCancelEnroll, onClose}: Props) {
  const locale = useLocale();
  const ui = messages[locale].ui;
  const d = ui.distribution;
  const info = signer.info;
  return <Modal id="signing-dialog" open={open} onCancel={onClose} labelledBy="signing-heading" locked={signer.enrolling} initialFocus="#signing-close"
    returnFocus={() => document.getElementById('export-signing')}>
    <div className="dialog-header"><div><h2 id="signing-heading">{d.signingHeading}</h2><p>{d.signingIntro}</p></div></div>
    <div className="dialog-body">
      {info && <>
        <dl id="signing-facts" className="run-identity">
          <dt>{d.provider}</dt><dd>{info.provider === 'google' ? d.providerGoogle : info.provider}</dd>
          <dt>{d.accountLabel}</dt><dd id="signing-account">{info.display_label}</dd>
          <dt>{d.keyFingerprint}</dt><dd id="signing-key" className="mono">{info.key_id}</dd>
          <dt>{d.enrolledAt}</dt><dd>{enrolledFormats[locale].format(info.enrolled_at * 1000)}</dd>
        </dl>
        <p className="muted">{d.localOnly}</p>
      </>}
      {info === null && signer.statusError === null && <p id="signing-none">{d.noKey}</p>}
      {info === undefined && signer.statusError === null && <p className="muted" role="status">{d.checking}</p>}
      {signer.statusError && <FaultMessage title={d.statusFailed} value={signer.statusError}/>}
      {signer.enrolling && <p id="signing-progress" role="status">{d.enrolling}</p>}
      {signer.enrollError && <FaultMessage title={d.enrollFailed} value={signer.enrollError}/>}
      {signer.cancelError && <FaultMessage title={d.cancelEnroll} value={signer.cancelError}/>}
      <p className="inline-warning">{d.custody}</p>
    </div>
    <div className="dialog-footer">
      <span id="signing-status" role="status">{signer.cancelling ? d.cancelling : info === null && !signer.enrolling ? enrollBlock ?? '' : ''}</span>
      {signer.enrolling
        ? <button id="signing-cancel" type="button" disabled={signer.cancelling} onClick={onCancelEnroll}>{d.cancelEnroll}</button>
        : info === null && signer.statusError === null
          && <button id="signing-enroll" type="button" className="primary" disabled={enrollBlock !== null} onClick={onEnroll}>{d.enroll}</button>}
      <button id="signing-close" type="button" disabled={signer.enrolling} onClick={onClose}>{ui.common.close}</button>
    </div>
  </Modal>;
}

import {useEffect, useLayoutEffect, useRef, useState} from 'react';
import {FaultMessage, fault} from './ResultPanel.tsx';
import {INITIAL_SHARE, ProfileShareSession} from '../profile-share.ts';
import type {DraftStamp, ShareClient, ShareContext, ShareState, SharedProfile} from '../profile-share.ts';
import {messages} from '../i18n.ts';
import {useLocale} from '../locale.tsx';

export interface ShareHandlers {
  client:ShareClient;
  // Replaces the reviewed Tab's draft only while it still matches `reviewed`; nothing is saved or started.
  adopt:(shared:SharedProfile, reviewed:DraftStamp) => void;
}

export interface ShareControls {state:ShareState; session:ProfileShareSession}

// The Run page is keyed by Tab and revision, so leaving it ends this session and drops pending replies.
export function useProfileShare(client:ShareClient, context:ShareContext):ShareControls {
  const clientRef = useRef(client);
  clientRef.current = client;
  const [state, setState] = useState<ShareState>(INITIAL_SHARE);
  const [session] = useState(() => new ProfileShareSession({
    exportCode:(id, includeName) => clientRef.current.exportCode(id, includeName),
    decode:code => clientRef.current.decode(code),
    copy:code => clientRef.current.copy(code),
  }, context, setState));
  const {draft} = context;
  // Before paint, so a withdrawn preview is never shown against the changed draft.
  useLayoutEffect(() => session.observe(context),
    [session, context.owner, context.source, context.packageId, context.schemaIdentity, draft.draftRevision, draft.selectedId, draft.name]);
  useEffect(() => () => session.close(), [session]);
  return {state, session};
}

interface Props {
  share:ShareControls; locked:boolean;
  // The open draft differs from the saved profile that export reads.
  draftDiffers:boolean;
  replacesEdits:boolean;
  onAdopt:ShareHandlers['adopt'];
}

export default function ProfileSharePanels({share, locked, draftDiffers, replacesEdits, onAdopt}:Props) {
  const locale = useLocale();
  const t = messages[locale].ui;
  const s = t.profileShare;
  const {state, session} = share;
  const review = state.export;
  const incoming = state.import;
  const preview = incoming?.preview ?? null;
  function use() {
    if (locked) return;
    const taken = session.take();
    if (taken) onAdopt(taken.shared, taken.reviewed);
  }
  return <>
    {review && <section id="profile-share-export-review" className="profile-share" aria-labelledby="profile-share-export-heading">
      <h3 id="profile-share-export-heading">{s.exportHeading} · {review.profileName}</h3>
      {draftDiffers && <p className="muted">{s.unsavedExcluded}</p>}
      <label className="checkbox-label"><input id="profile-share-include-name" type="checkbox" checked={review.includeName} disabled={locked}
        onChange={event => session.includeName(event.target.checked)}/>{s.includeName}</label>
      {review.error !== null
        ? <div id="profile-share-export-error"><FaultMessage title={s.exportFailed} value={fault(review.error)}/></div>
        : <div className="field"><label htmlFor="profile-share-code">{s.code}</label>
          <textarea id="profile-share-code" readOnly rows={4} spellCheck={false} value={review.code ?? ''}
            placeholder={review.pending ? s.preparing : undefined} aria-busy={review.pending}/></div>}
      <p id="profile-share-privacy" className="inline-warning">{s.privacy}</p>
      <div className="button-row">
        <button id="profile-share-copy" type="button" className="primary" disabled={locked || review.code === null}
          aria-describedby="profile-share-privacy" onClick={() => void session.copy()}>{s.copy}</button>
        <button id="profile-share-close" type="button" onClick={() => session.closeExport()}>{t.common.cancel}</button>
        <span id="profile-share-copied" className="muted" role="status">{review.copied ? s.copied : ''}</span>
      </div>
      {review.copyError !== null && <div id="profile-share-copy-error"><FaultMessage title={s.copyFailed} value={fault(review.copyError)}/></div>}
    </section>}
    {incoming && <section id="profile-share-import-review" className="profile-share" aria-labelledby="profile-share-import-heading">
      <h3 id="profile-share-import-heading">{s.importHeading}</h3>
      <div className="field"><label htmlFor="profile-share-input">{s.code}</label>
        <textarea id="profile-share-input" rows={4} spellCheck={false} autoComplete="off" autoFocus value={incoming.text}
          placeholder={s.inputPlaceholder} onChange={event => session.edit(event.target.value)}/></div>
      {incoming.stale && <p id="profile-share-stale" className="inline-warning">{s.stale}</p>}
      {incoming.error !== null && <div id="profile-share-decode-error"><FaultMessage title={s.decodeFailed} value={fault(incoming.error)}/></div>}
      {preview && <div id="profile-share-preview">
        <h4>{s.previewHeading}</h4>
        <dl className="run-identity">
          <dt>{t.distribution.packageId}</dt><dd className="mono">{preview.package_id}</dd>
          <dt>{s.schemaIdentity}</dt><dd className="mono">{preview.schema_identity}</dd>
          <dt>{s.suggestedName}</dt><dd id="profile-share-name">{preview.name ?? s.noName}</dd>
        </dl>
        <span className="eyebrow">{s.values}</span>
        <pre id="profile-share-values">{JSON.stringify(preview.values, null, 2)}</pre>
        {replacesEdits && <p id="profile-share-replaces" className="inline-warning">{s.replacesEdits}</p>}
      </div>}
      <div className="button-row">
        {preview
          ? <button id="profile-share-use" type="button" className="primary" disabled={locked}
            aria-describedby={replacesEdits ? 'profile-share-replaces' : undefined} onClick={use}>{s.use}</button>
          : <button id="profile-share-decode" type="button" className="primary" disabled={locked || incoming.pending || incoming.text.trim() === ''}
            onClick={() => void session.decode()}>{incoming.pending ? s.decoding : s.decode}</button>}
        <button id="profile-share-cancel" type="button" onClick={() => session.cancelImport()}>{t.common.cancel}</button>
      </div>
    </section>}
  </>;
}

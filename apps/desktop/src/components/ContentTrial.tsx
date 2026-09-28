import {useEffect, useRef, useState} from 'react';
import type {ReactNode} from 'react';
import {CONTENT_ASPECTS, detectDisplayedContent} from '../contentDetection.ts';
import type {ContentAspect} from '../contentDetection.ts';
import {useLocale} from '../locale.tsx';
import {messages} from '../i18n.ts';
import type {PixelRect} from '../recognition.ts';
import Select from './Select.tsx';

interface Proposal {rect:PixelRect; aspect?:string; approximate:boolean}
type Outcome = {identity:string; request:number} & (
  | {kind:'busy' | 'none' | 'ambiguous' | 'failed'}
  | {kind:'candidate'; proposal:Proposal}
);
interface Props {
  identity:string; active:boolean; disabled:boolean; width:number; height:number;
  content:PixelRect|null; image:string|null;
  getImage:() => HTMLImageElement|null;
  isCurrent:() => boolean;
  onApply:(content:PixelRect) => void;
  children:(candidate:PixelRect|null, clearCandidate:() => void, controls:ReactNode, feedback:ReactNode) => ReactNode;
}

// A proposal owns no authority or original pixels. Only explicit Apply emits the existing
// content edit. Keep this component AND its image child mounted when identity changes.
export default function ContentTrial({identity, active, disabled, width, height, content, image,
  getImage, isCurrent, onApply, children}:Props) {
  const r = messages[useLocale()].ui.recognition;
  const [aspect, setAspect] = useState<ContentAspect>('auto');
  const [outcome, setOutcome] = useState<Outcome|null>(null);
  const request = useRef(0);
  const mounted = useRef(true);
  const latest = useRef({identity, active, disabled, isCurrent});
  latest.current = {identity, active, disabled, isCurrent};
  const available = active && !disabled && Boolean(image && content) && width > 0 && height > 0;
  const current = available && outcome?.identity === identity && outcome.request === request.current ? outcome : null;
  const proposal = current?.kind === 'candidate' ? current.proposal : null;
  const busy = current?.kind === 'busy';

  function clear() { request.current++; setOutcome(null); }
  useEffect(() => { clear(); }, [identity]);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; request.current++; };
  }, []);

  function accepts(ticket:number, stamp:string):boolean {
    const now = latest.current;
    return mounted.current && request.current === ticket && now.identity === stamp && now.active && !now.disabled && now.isCurrent();
  }

  async function detect() {
    if (!available || !image || !isCurrent()) return;
    const ticket = ++request.current, stamp = identity;
    setOutcome({identity: stamp, request: ticket, kind: 'busy'});
    // Publish feedback first. Cancel, manual edits or a queued capture can invalidate this task.
    await new Promise<void>(resolve => setTimeout(resolve, 0));
    if (!accepts(ticket, stamp)) return;
    try {
      const result = detectDisplayedContent(getImage(), image, aspect, width, height);
      if (accepts(ticket, stamp)) setOutcome({identity: stamp, request: ticket, ...result});
    } catch {
      if (accepts(ticket, stamp)) setOutcome({identity: stamp, request: ticket, kind: 'failed'});
    }
  }

  function fullImage() {
    if (!available || !isCurrent()) return;
    setOutcome({identity, request: ++request.current, kind: 'candidate',
      proposal: {rect: {x: 0, y: 0, width, height}, approximate: false}});
  }

  const unchanged = proposal && content && proposal.rect.x === content.x && proposal.rect.y === content.y
    && proposal.rect.width === content.width && proposal.rect.height === content.height;
  const status = busy ? r.contentDetecting : current?.kind === 'none' ? r.contentDetectEmpty
    : current?.kind === 'ambiguous' ? r.contentDetectAmbiguous : current?.kind === 'failed' ? r.contentDetectFailed
      : proposal ? proposal.approximate ? r.contentCandidateApproximate : r.contentCandidate : '';
  const controls = active && <div className="content-trial" role="group" aria-label={r.contentTrialTitle} title={r.contentTrialTitle}
    onKeyDown={event => {if (event.key === 'Escape') {event.preventDefault(); event.stopPropagation(); clear();}}}>
    <div className="segmented">
      <Select id="content-trial-aspect" aria-label={r.contentPresetLabel} value={aspect} disabled={!available}
        options={[{value: 'auto', label: r.contentAspectAuto}, ...CONTENT_ASPECTS.map(value => ({value, label: value}))]}
        onChange={value => {clear(); setAspect(value as ContentAspect);}}/>
      <button id="content-trial-detect" type="button" disabled={!available || busy} onClick={() => void detect()}>{r.contentDetect}</button>
      <button id="content-trial-full" type="button" disabled={!available} onClick={fullImage}>{r.contentFullImage}</button>
    </div>
    <div className="segmented">
      <button id="content-trial-apply" type="button" disabled={!proposal || Boolean(unchanged)} onClick={() => {
        if (!proposal || !current || !accepts(current.request, current.identity)) return;
        clear();
        onApply(proposal.rect);
      }}>{r.contentApply}</button>
      <button id="content-trial-cancel" type="button" disabled={!current} onClick={clear}>{r.contentCancel}</button>
    </div>
  </div>;
  const feedback = active && <span className="content-trial-status" role="status" aria-live="polite"
    title={[status, proposal?.approximate ? r.contentDetectApproximate : ''].filter(Boolean).join(' · ')}>
    {status}{proposal && ` · ${proposal.aspect ? `${proposal.aspect} · ` : ''}x=${proposal.rect.x}, y=${proposal.rect.y}, ${proposal.rect.width} × ${proposal.rect.height}`}
  </span>;
  return children(proposal?.rect ?? null, clear, controls, feedback);
}

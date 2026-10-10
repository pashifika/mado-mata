import type {PreviewAction, PreviewSnapshot} from './recognition.ts';

// Effect invalidation revokes publication, not ownership of an outstanding host request.
export class PreviewImageLoad {
  private outstanding = 0;
  private generation = 0;
  private settledEpoch:number|null = null;

  pending(epoch:number|null):boolean {
    return this.outstanding > 0 || (epoch !== null && this.settledEpoch !== epoch);
  }

  begin(epoch:number):{settle:() => void; invalidate:() => void} {
    const generation = ++this.generation;
    let active = true;
    let settled = false;
    this.outstanding++;
    this.settledEpoch = null;
    return {
      settle:() => {
        if (settled) return;
        settled = true;
        this.outstanding--;
        if (active && generation === this.generation) this.settledEpoch = epoch;
      },
      invalidate:() => {
        active = false;
        if (generation === this.generation) this.settledEpoch = null;
      },
    };
  }
}

export function previewActionAvailable(snapshot:PreviewSnapshot|null, closing:boolean, loading:boolean, action:PreviewAction):boolean {
  if (!snapshot || closing) return false;
  if (action === 'cancel') return true;
  return !loading && snapshot.editable && !snapshot.commandBusy && !snapshot.running
    && !snapshot.nativeSelection?.busy && snapshot.nativeSelection?.platform !== 'unsupported';
}

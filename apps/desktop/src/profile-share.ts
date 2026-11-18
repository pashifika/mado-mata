import {editDraft} from './workspace.ts';
import type {Bound, Derived} from './workspace.ts';
import type {Json, Profile} from './types.ts';

// Host projection of one decoded `MMP1:` code; `name` is only a suggestion.
export interface SharedProfile {package_id:string; schema_identity:string; values:Record<string,Json>; name?:string}

// Bound to one Tab revision. Nothing here saves, starts or reads the clipboard.
export interface ShareClient {
  exportCode:(id:string, includeName:boolean) => Promise<string>;
  decode:(code:string) => Promise<SharedProfile>;
  copy:(code:string) => Promise<void>;
}

// The receiving draft a decode was reviewed against. Name edits and Save/Delete do not advance `draftRevision`.
export interface DraftStamp {draftRevision:number; selectedId:string|null; name:string}

// `source` is the selected saved record by identity; a fresh catalog object withdraws its export.
export interface ShareContext {owner:string; source:Profile|null; packageId:string; schemaIdentity:string; draft:DraftStamp}

export interface ExportReview {
  profileName:string; includeName:boolean; pending:boolean; code:string|null; error:unknown;
  copied:boolean; copyError:unknown;
}

export interface ImportReview {
  text:string; pending:boolean; preview:SharedProfile|null; error:unknown; reviewed:DraftStamp|null;
  // A preview or pending decode was withdrawn by a draft, owner or schema change.
  stale:boolean;
}

export interface ShareState {export:ExportReview|null; import:ImportReview|null}

export const INITIAL_SHARE:ShareState = {export:null, import:null};

export function shareContext(owner:string, bound:Bound, facts:Derived):ShareContext {
  return {owner, source:facts.bound ? facts.selectedProfile ?? null : null,
    packageId:bound.package.package_id, schemaIdentity:bound.package.schema_identity,
    draft:{draftRevision:bound.draftRevision, selectedId:bound.selectedId, name:bound.name}};
}

function sameDraft(left:DraftStamp, right:DraftStamp):boolean {
  return left.draftRevision === right.draftRevision && left.selectedId === right.selectedId && left.name === right.name;
}

// Every request takes a fresh ticket and a reply applies only while it is current, so cancel, edit, context change
// and close drop late replies.
export class ProfileShareSession {
  private state:ShareState = INITIAL_SHARE;
  private context:ShareContext;
  private serial = 0;
  private exportTicket = 0;
  private copyTicket = 0;
  private decodeTicket = 0;
  private client:ShareClient;
  private publish:(state:ShareState) => void;

  constructor(client:ShareClient, context:ShareContext, publish:(state:ShareState) => void) {
    this.client = client;
    this.context = context;
    this.publish = publish;
  }

  // Export reads the saved record, so draft edits leave its review alone.
  observe(context:ShareContext):void {
    const previous = this.context;
    this.context = context;
    let next = this.state;
    if (context.owner !== previous.owner || context.source !== previous.source) {
      this.exportTicket = 0;
      this.copyTicket = 0;
      if (next.export) next = {...next, export:null};
    }
    if (context.owner !== previous.owner || context.packageId !== previous.packageId
      || context.schemaIdentity !== previous.schemaIdentity || !sameDraft(context.draft, previous.draft)) {
      this.decodeTicket = 0;
      const review = next.import;
      if (review && (review.pending || review.preview)) {
        next = {...next, import:{...review, pending:false, preview:null, error:null, reviewed:null, stale:true}};
      }
    }
    if (next !== this.state) this.set(next);
  }

  openExport():void {
    if (this.state.export === null) void this.requestExport(false);
  }

  includeName(value:boolean):void {
    if (this.state.export !== null && this.state.export.includeName !== value) void this.requestExport(value);
  }

  // Copies exactly the displayed code; only the latest copy of the current code is reported.
  async copy():Promise<void> {
    const code = this.state.export?.code;
    if (code == null) return;
    const exported = this.exportTicket;
    const ticket = ++this.serial;
    this.copyTicket = ticket;
    this.setExport({copied:false, copyError:null});
    let copyError:unknown = null;
    try {
      await this.client.copy(code);
    } catch (error) {
      copyError = error;
    }
    if (ticket === this.copyTicket && exported === this.exportTicket) this.setExport({copied:copyError === null, copyError});
  }

  closeExport():void {
    this.exportTicket = 0;
    this.copyTicket = 0;
    if (this.state.export) this.set({...this.state, export:null});
  }

  openImport():void {
    if (this.state.import === null) this.set({...this.state, import:{text:'', pending:false, preview:null, error:null, reviewed:null, stale:false}});
  }

  edit(text:string):void {
    if (!this.state.import) return;
    this.decodeTicket = 0;
    this.set({...this.state, import:{text, pending:false, preview:null, error:null, reviewed:null, stale:false}});
  }

  // The host owns parsing, bounds and numeric meaning, so the pasted text is sent unchanged.
  async decode():Promise<void> {
    const review = this.state.import;
    if (!review || review.pending || review.text.trim() === '') return;
    const ticket = ++this.serial;
    this.decodeTicket = ticket;
    this.setImport({pending:true, preview:null, error:null, reviewed:this.context.draft, stale:false});
    try {
      const preview = await this.client.decode(review.text);
      if (ticket === this.decodeTicket) this.setImport({pending:false, preview});
    } catch (error) {
      if (ticket === this.decodeTicket) this.setImport({pending:false, error, reviewed:null});
    }
  }

  cancelImport():void {
    this.decodeTicket = 0;
    if (this.state.import) this.set({...this.state, import:null});
  }

  take():{shared:SharedProfile; reviewed:DraftStamp}|null {
    const review = this.state.import;
    if (!review?.preview || review.reviewed === null) return null;
    const taken = {shared:review.preview, reviewed:review.reviewed};
    this.cancelImport();
    return taken;
  }

  close():void {
    this.exportTicket = 0;
    this.copyTicket = 0;
    this.decodeTicket = 0;
  }

  private async requestExport(includeName:boolean):Promise<void> {
    const source = this.context.source;
    if (source === null) return;
    const ticket = ++this.serial;
    this.exportTicket = ticket;
    this.copyTicket = 0;
    this.set({...this.state, export:{profileName:source.name, includeName, pending:true, code:null, error:null, copied:false, copyError:null}});
    try {
      const code = await this.client.exportCode(source.id, includeName);
      if (ticket === this.exportTicket) this.setExport({pending:false, code});
    } catch (error) {
      if (ticket === this.exportTicket) this.setExport({pending:false, error});
    }
  }

  private setExport(next:Partial<ExportReview>):void {
    if (this.state.export) this.set({...this.state, export:{...this.state.export, ...next}});
  }

  private setImport(next:Partial<ImportReview>):void {
    if (this.state.import) this.set({...this.state, import:{...this.state.import, ...next}});
  }

  private set(next:ShareState):void {
    this.state = next;
    this.publish(next);
  }
}

// An unbound draft of this Tab only: with no saved ID, Save creates a new profile even under a matching name.
// Null when the draft changed since review or the preview belongs to another package or schema.
export function adoptSharedProfile(bound:Bound, shared:SharedProfile, reviewed:DraftStamp):Bound|null {
  if (!sameDraft({draftRevision:bound.draftRevision, selectedId:bound.selectedId, name:bound.name}, reviewed)
    || shared.package_id !== bound.package.package_id || shared.schema_identity !== bound.package.schema_identity) return null;
  return editDraft({...bound, selectedId:null, preset:'', name:shared.name ?? ''}, structuredClone(shared.values));
}

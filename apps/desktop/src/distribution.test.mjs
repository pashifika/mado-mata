import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AuthoringController} from './authoring-controller.ts';
import {applySave, beginPending, editFile, openSession, saveTicket, undoFile} from './authoring.ts';
import {saveFiles} from './authoring-publication.ts';
import {acceptExportReview, acceptImportReview, declaredRelease, exportConfirmable, exportReadiness, failExportPreparation, publicationOutcome,
  suggestedName} from './distribution.ts';

const owner={workspace:{workspace_id:'a',revision:1},token:'authoring-1-1'};
const MANIFEST='{"id":"example.a","distribution":{"version":1,"release_version":"1.2.0-rc.1+build.5"}}\n';
const MAIN='export const a = 1;\n';
function file(path,kind,text){
  return {path,kind,text,bytes:text===null?4:new TextEncoder().encode(text).length};
}
function packageView({token=owner.token,revision='rev-1',main=MAIN,managed=false}={}){
  return {owner:{...owner,token},package_path:'/private/packages/a',package_id:'example.a',revision,managed,
    files:[file('package.json','manifest',MANIFEST),file('main.ts','source',main),file('images/logo.png','asset',null)]};
}
function setup(view=packageView()){
  const controller=new AuthoringController(()=>{});
  controller.update(()=>openSession(view));
  return controller;
}
// Human typing at the draft's caret, as the editor reports it.
function type(controller,path,inserted){
  controller.update(session=>{
    const current=session.drafts.get(path);
    const at=current.range;
    const caret=at.start+inserted.length;
    return editFile(session,path,{text:current.text.slice(0,at.start)+inserted+current.text.slice(at.end),start:caret,end:caret},at,{type:'insertText',data:inserted,composing:false});
  });
}
function review(ticket,extra={}){
  return {package_id:'example.a',release_version:'1.2.0-rc.1+build.5',name:'Example',description:'<b>text</b>',game_names:{en:'Game'},key_id:'k'.repeat(64),
    revision:ticket.revision,artifact_id:'a'.repeat(64),files:3,bytes:120,token:'review-1',workspace:ticket.owner.workspace,owner:ticket.owner,
    destination:'/exports/example.a-1.2.0-rc.1+build.5.mmpkg',runtime:'mado-host-v1',signature_valid:true,account_verified:false,...extra};
}

test('an export capture needs clean saved drafts and goes stale for good after any later contributing edit',()=>{
  const controller=setup();
  const ticket=controller.exportTicket();
  assert.ok(ticket);
  assert.equal(controller.exportCurrent(ticket),true);
  type(controller,'main.ts','// draft\n');
  assert.equal(controller.exportTicket(),null);
  assert.equal(controller.exportCurrent(ticket),false);
  // Undoing back to the saved text is clean again, yet the reviewed capture never regains publication authority.
  controller.update(session=>undoFile(session,'main.ts'));
  assert.equal(controller.current().drafts.get('main.ts').text,MAIN);
  const fresh=controller.exportTicket();
  assert.ok(fresh);
  assert.equal(controller.exportCurrent(ticket),false);
  assert.equal(controller.exportCurrent(fresh),true);
  // A command that changes no contributing draft (validation) neither stales a review nor admits a new capture.
  controller.update(session=>beginPending(session,{kind:'validate'}));
  assert.equal(controller.exportTicket(),null);
  assert.equal(controller.exportCurrent(fresh),true);
});

test('saved revision, Edit owner and managed releases bound export captures',()=>{
  const controller=setup();
  const ticket=controller.exportTicket();
  type(controller,'main.ts','// saved\n');
  const save=saveTicket(controller.current(),'main.ts');
  controller.update(session=>applySave(session,save,{owner,committed_revision:'rev-2',view:packageView({revision:'rev-2',main:save.text}),refresh_error:null}));
  const saved=controller.exportTicket();
  assert.equal(saved.revision,'rev-2');
  assert.equal(controller.exportCurrent(ticket),false);
  // A successor lease for the same Workspace and package is another owner: the earlier capture cannot publish into it.
  controller.update(()=>openSession(packageView({token:'authoring-1-2',revision:'rev-2',main:save.text})));
  assert.equal(controller.exportCurrent(saved),false);
  assert.notEqual(controller.exportTicket(),null);
  // A verified installed release is copy-before-edit: no capture, and agent edits are refused like the person's.
  const managed=setup(packageView({managed:true}));
  assert.equal(managed.exportTicket(),null);
  const description=managed.describe();
  const main=description.resources.find(item=>item.path==='main.ts');
  const response=managed.handle({id:'e',owner:description.owner,package:description.package,
    operation:{kind:'edit',edits:[{resource:main.id,version:main.version,text:'changed'}]}});
  assert.equal(response.ok,false);
  assert.equal(response.error.code,'managed_package');
  assert.equal(managed.current().drafts.get('main.ts').text,MAIN);
});

test('a delayed Save reply keeps the later draft dirty and stops export before preparation',async()=>{
  const controller=setup();
  type(controller,'main.ts','// A\n');
  const savedText=controller.current().drafts.get('main.ts').text;
  let release;
  const gate=new Promise(resolve=>{release=resolve;});
  const ports={
    async save(current,ticket){
      await gate;
      return {owner:current,committed_revision:'rev-2',view:packageView({revision:'rev-2',main:ticket.text}),refresh_error:null};
    },
    refreshRecognition:async()=>true,
    fault:cause=>({category:'Test',message:String(cause),context:null}),
    available:()=>true,
  };
  const targets=controller.describe().resources.filter(item=>item.dirty).map(item=>({resource:item.id,version:item.version}));
  const saving=saveFiles(controller,targets,ports);
  type(controller,'main.ts','// B\n');
  release();
  const sequence=await saving;
  assert.deepEqual(sequence.committed,[{path:'main.ts',revision:'rev-2'}]);
  assert.equal(sequence.failure,null);
  const draft=controller.current().drafts.get('main.ts');
  assert.equal(draft.base,savedText);
  assert.equal(draft.text,'// A\n// B\n'+MAIN);
  const step=exportReadiness(controller,owner,sequence);
  assert.equal(step.kind,'stopped');
  assert.equal(step.remaining,true);
  assert.equal(step.sequence,sequence);
  // Nothing in deciding readiness touched the newer draft.
  assert.equal(controller.current().drafts.get('main.ts'),draft);
  // Without Save the same dirty state asks for explicit resolution rather than exporting stale disk bytes.
  assert.deepEqual(exportReadiness(controller,owner,null),{kind:'drafts'});
});

test('a Save failure stops export with its committed prefix and remaining drafts',()=>{
  const controller=setup();
  const failure={path:'main.ts',committedRevision:null,refreshRequired:false,error:{category:'AuthoringConflict',message:'changed',context:null}};
  const sequence={committed:[{path:'package.json',revision:'rev-2'}],remaining:[{resource:'r',version:'v'}],failure};
  assert.deepEqual(exportReadiness(controller,owner,sequence),{kind:'stopped',sequence,remaining:true});
  assert.deepEqual(exportReadiness(controller,owner,{committed:[],remaining:[],failure:null}),{kind:'ready',note:null});
});

test('preparation replies apply only to their own attempt, owner and saved revision',()=>{
  const controller=setup();
  const ticket=controller.exportTicket();
  const preparing={owner,step:{kind:'preparing',attempt:2,ticket,destination:'/exports/a.mmpkg',cancelling:false,cancelError:null}};
  const valid=review(ticket);
  // A late reply of a cancelled or superseded attempt changes nothing; a closed dialog stays closed.
  assert.equal(acceptExportReview(preparing,1,valid),preparing);
  assert.equal(acceptExportReview(null,2,valid),null);
  assert.equal(failExportPreparation(preparing,1,{category:'PackageCancelled',message:'cancelled',context:null}),preparing);
  for(const mismatch of [review(ticket,{owner:{...owner,token:'authoring-1-2'}}),review(ticket,{revision:'rev-0'}),
    review(ticket,{workspace:{workspace_id:'a',revision:2}}),review(ticket,{owner:null})]){
    const refused=acceptExportReview(preparing,2,mismatch);
    assert.equal(refused.step.kind,'ready');
    assert.equal(refused.step.note.error.category,'DistributionReviewMismatch');
  }
  const accepted=acceptExportReview(preparing,2,valid);
  assert.deepEqual(accepted.step,{kind:'review',ticket,review:valid});
  assert.equal(exportConfirmable(accepted,controller),true);
  // A draft created after review removes confirmation authority without blocking the edit itself.
  type(controller,'main.ts','// later\n');
  assert.equal(controller.current().drafts.get('main.ts').text,'// later\n'+MAIN);
  assert.equal(exportConfirmable(accepted,controller),false);
});

test('import reviews stay bound to the originating Tab session',()=>{
  const workspace={workspace_id:'b',revision:3};
  const preparing={workspace,step:{kind:'preparing',attempt:4,path:'/downloads/a.mmpkg',cancelling:false,cancelError:null}};
  const facts={...review({owner,revision:'rev-1'}),workspace,owner:null};
  assert.deepEqual(acceptImportReview(preparing,4,facts).step,{kind:'review',review:facts});
  assert.equal(acceptImportReview(preparing,3,facts),preparing);
  assert.equal(acceptImportReview(preparing,4,{...facts,workspace:{workspace_id:'b',revision:4}}).step.kind,'failed');
  assert.equal(acceptImportReview(preparing,4,{...facts,owner}).step.kind,'failed');
});

test('publication receipts distinguish committed cleanup failures, reuse and foreign artifacts',()=>{
  const facts=review({owner,revision:'rev-1'});
  const receipt={path:'/data/pkgs/example.a/'+facts.artifact_id,package_id:facts.package_id,release_version:facts.release_version,artifact_id:facts.artifact_id,
    unchanged:false,committed:true,cleanup_error:null};
  assert.equal(publicationOutcome(facts,receipt),'clean');
  assert.equal(publicationOutcome(facts,{...receipt,committed:false,unchanged:true}),'unchanged');
  assert.equal(publicationOutcome(facts,{...receipt,cleanup_error:{category:'Io',message:'staging remains',context:null}}),'cleanup');
  assert.equal(publicationOutcome(facts,{...receipt,committed:false}),'uncommitted');
  assert.equal(publicationOutcome(facts,{...receipt,artifact_id:'b'.repeat(64)}),'mismatch');
  assert.equal(publicationOutcome(facts,{...receipt,release_version:'1.2.0'}),'mismatch');
});

test('the saved manifest release names the suggested export file without inventing one',()=>{
  assert.equal(declaredRelease(MANIFEST),'1.2.0-rc.1+build.5');
  assert.equal(declaredRelease('{"id":"example.a"}'),null);
  assert.equal(declaredRelease('{not json'),null);
  assert.equal(declaredRelease(null),null);
  assert.equal(suggestedName('example.a','1.2.0'),'example.a-1.2.0.mmpkg');
  assert.equal(suggestedName('example.a',null),'example.a.mmpkg');
});

test('cancellation wins over a successful preparation reply for both transfer dialogs',()=>{
  const controller=setup();
  const ticket=controller.exportTicket();
  const facts=review(ticket);
  const exporting={owner,step:{kind:'preparing',attempt:5,ticket,destination:'/exports/a.mmpkg',cancelling:true,cancelError:null}};
  const cancelled=acceptExportReview(exporting,5,facts);
  assert.equal(cancelled.step.kind,'ready');
  assert.equal(cancelled.step.note.error.category,'PackageCancelled');
  assert.equal(exportConfirmable(cancelled,controller),false);
  const importing={workspace:owner.workspace,step:{kind:'preparing',attempt:6,path:'/downloads/a.mmpkg',cancelling:true,cancelError:null}};
  const imported=acceptImportReview(importing,6,{...facts,owner:null});
  assert.equal(imported.step.kind,'failed');
  assert.equal(imported.step.error.category,'PackageCancelled');
});

test('maximum-length valid metadata still produces a portable export filename',()=>{
  const id='p'.repeat(128);
  const release=`1.0.0+${'b'.repeat(122)}`;
  const name=suggestedName(id,release);
  assert.equal(name,`${id}.mmpkg`);
  assert.ok(new TextEncoder().encode(name).length<=255);
  const boundary=suggestedName(id,`1.0.0+${'b'.repeat(114)}`);
  assert.equal(new TextEncoder().encode(boundary).length,255);
});

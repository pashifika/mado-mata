import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AUTHORING_CONFLICT,applyCatalogMutation,applyRecognitionMutation,applyRefresh,applySave,applyValidation,beginComposition,beginPending,catalogBlock,catalogTicket,diagnosticLocation,discardFile,editFile,endComposition,failCommand,fileDirty,findMatch,matchSummary,offsetAt,openSession,recoveryPath,redoFile,saveBlock,saveTicket,selectFile,selectRecognition,undoFile,validationCurrent,validationTicket} from './authoring.ts';
import {applyAuthoringExit,applyInvalidatedViews,editDraft,updateBound,workspaceFromView} from './workspace.ts';

const owner={workspace:{workspace_id:'a',revision:1},token:'lease-1'};
const MAIN='export const a = 1;\n';
const HELPER='export const b = 2;\n';
function file(path,kind,text){
  return {path,kind,text,bytes:text===null?4:new TextEncoder().encode(text).length};
}
function packageView(revision,files,token='lease-1'){
  return {owner:{...owner,token},package_path:'/pkg/a',package_id:'example.a',revision,files};
}
const files=[file('package.json','manifest','{}\n'),file('main.ts','source',MAIN),file('helper.ts','source',HELPER),file('schema.json','schema','{}\n'),file('images/logo.png','asset',null)];
function withText(path,text,list=files){
  return list.map(item=>item.path===path?file(path,item.kind,text):item);
}
function mutation(revision,view,refreshError=null){
  return {owner,committed_revision:revision,view,refresh_error:refreshError};
}
// Inserts at the draft's caret exactly as the textarea reports it: new text/selection, the selection before the input.
function type(state,path,inserted,inputType='insertText',before){
  const draft=state.drafts.get(path);
  const at=before??draft.range;
  const text=draft.text.slice(0,at.start)+inserted+draft.text.slice(at.end);
  const caret=at.start+inserted.length;
  return editFile(state,path,{text,start:caret,end:caret},at,{type:inputType,data:inserted,composing:false});
}
const text=(state,path)=>state.drafts.get(path).text;

test('each file keeps its own text, dirty state, caret and undo history across file switches',()=>{
  let state=openSession(packageView('rev-1',files));
  assert.equal(state.selected,'main.ts');
  state=type(state,'main.ts','A');
  state=selectFile(state,'helper.ts',{start:1,end:1});
  state=type(state,'helper.ts','B');
  state=selectFile(state,'main.ts',{start:1,end:1});
  assert.deepEqual(state.drafts.get('main.ts').range,{start:1,end:1});
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),MAIN);
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(text(state,'helper.ts'),'B'+HELPER);
  assert.equal(fileDirty(state.drafts.get('helper.ts')),true);
  assert.equal(state.drafts.get('helper.ts').undo.length,1);
  state=redoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'A'+MAIN);
  assert.deepEqual(state.drafts.get('main.ts').range,{start:1,end:1});
  assert.equal(undoFile(openSession(packageView('rev-1',files)),'main.ts').drafts.get('main.ts').text,MAIN);
});

test('returning from recognition to the same file restores its unsaved text and caret',()=>{
  let state=type(openSession(packageView('rev-1',files)),'main.ts','draft');
  state=selectRecognition(state,{start:2,end:4});
  assert.equal(state.destination,'recognition');
  state=selectFile(state,'main.ts',null);
  assert.equal(state.destination,'file');
  assert.equal(text(state,'main.ts'),'draft'+MAIN);
  assert.deepEqual(state.drafts.get('main.ts').range,{start:2,end:4});
  assert.equal(fileDirty(state.drafts.get('main.ts')),true);
});

test('recognition publication preserves concurrent source and manifest drafts against the committed view',()=>{
  let state=type(type(openSession(packageView('rev-1',files)),'main.ts','source edit'),'package.json','manifest edit');
  state=selectRecognition(state,null);
  const saved=packageView('rev-2',[...withText('package.json','{"recognition":true}\n'),file('recognition/authoring.json','asset',null)]);
  state=applyRecognitionMutation(state,mutation('rev-2',saved));
  assert.equal(state.revision,'rev-2');
  assert.equal(state.destination,'recognition');
  assert.equal(text(state,'main.ts'),'source edit'+MAIN);
  assert.equal(text(state,'package.json'),'manifest edit{}\n');
  assert.equal(state.drafts.get('package.json').base,'{"recognition":true}\n');
  assert.equal(state.drafts.get('package.json').diskChanged,true);
  assert.equal(saveTicket(state,'main.ts').expected,'rev-2');
});

test('recognition commit remains authoritative after read failure and rejects a stale owner reply',()=>{
  const original=type(openSession(packageView('rev-1',files)),'main.ts','draft');
  const failedRead={category:'Io',message:'read failed',context:null};
  const state=applyRecognitionMutation(original,mutation('rev-2',null,failedRead));
  assert.equal(state.revision,'rev-2');
  assert.equal(state.refreshRequired,true);
  assert.equal(saveTicket(state,'main.ts'),null);
  assert.equal(text(state,'main.ts'),'draft'+MAIN);
  const successor=openSession(packageView('rev-next',files,'lease-next'));
  assert.equal(applyRecognitionMutation(successor,mutation('rev-2',null,failedRead)),successor);
});

test('typing forms word-sized undo steps; whitespace and a moved caret start new steps',()=>{
  let state=openSession(packageView('rev-1',files));
  for (const character of 'abc') state=type(state,'main.ts',character);
  state=type(state,'main.ts',' ');
  for (const character of 'de') state=type(state,'main.ts',character);
  // The caret moved to the end before typing again: the earlier word must not absorb this one.
  const end=text(state,'main.ts').length;
  state=type(state,'main.ts','z','insertText',{start:end,end});
  assert.equal(text(state,'main.ts'),'abc de'+MAIN+'z');
  const steps=['abc de'+MAIN,'abc '+MAIN,'abc'+MAIN,MAIN];
  for (const expected of steps) {
    state=undoFile(state,'main.ts');
    assert.equal(text(state,'main.ts'),expected);
  }
  assert.equal(state.drafts.get('main.ts').undo.length,0);
  assert.deepEqual(state.drafts.get('main.ts').range,{start:0,end:0});
  state=type(state,'main.ts','q');
  assert.equal(state.drafts.get('main.ts').redo.length,0);
});

test('IME composition records one undo step at composition end and refuses undo while composing',()=>{
  let state=openSession(packageView('rev-1',files));
  state=beginComposition(state,'main.ts',{start:0,end:0});
  for (const provisional of ['k','か','漢']) {
    state=editFile(state,'main.ts',{text:provisional+MAIN,start:provisional.length,end:provisional.length},{start:0,end:0},{type:'insertCompositionText',data:provisional,composing:true});
  }
  assert.equal(state.drafts.get('main.ts').undo.length,0);
  assert.equal(undoFile(state,'main.ts'),state);
  state=endComposition(state,'main.ts');
  // WebKit may deliver the committing input after compositionend; it belongs to the same step.
  state=editFile(state,'main.ts',{text:'漢字'+MAIN,start:2,end:2},{start:1,end:1},{type:'insertFromComposition',data:'漢字',composing:false});
  assert.equal(state.drafts.get('main.ts').undo.length,1);
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),MAIN);
  const cancelled=endComposition(beginComposition(openSession(packageView('rev-1',files)),'main.ts',{start:0,end:0}),'main.ts');
  assert.equal(cancelled.drafts.get('main.ts').undo.length,0);
});

test('a Save reply for draft A after draft B commits A without replacing or marking B saved',()=>{
  let state=type(openSession(packageView('rev-1',files)),'main.ts','A');
  const ticket=saveTicket(state,'main.ts');
  assert.deepEqual({expected:ticket.expected,text:ticket.text},{expected:'rev-1',text:'A'+MAIN});
  state=beginPending(state,{kind:'save',path:'main.ts'});
  state=type(state,'main.ts','B');
  state=applySave(state,ticket,mutation('rev-2',packageView('rev-2',withText('main.ts','A'+MAIN))));
  const draft=state.drafts.get('main.ts');
  assert.equal(state.revision,'rev-2');
  assert.equal(state.pending,null);
  assert.equal(draft.text,'AB'+MAIN);
  assert.equal(draft.base,'A'+MAIN);
  assert.equal(fileDirty(draft),true);
  assert.equal(state.notice.key,'authoringEarlierSaved');
  const next=saveTicket(state,'main.ts');
  assert.deepEqual({expected:next.expected,text:next.text},{expected:'rev-2',text:'AB'+MAIN});
  // Without a later edit the same reply marks the draft saved.
  const current=type(openSession(packageView('rev-1',files)),'main.ts','A');
  const saved=applySave(current,saveTicket(current,'main.ts'),mutation('rev-2',packageView('rev-2',withText('main.ts','A'+MAIN))));
  assert.equal(fileDirty(saved.drafts.get('main.ts')),false);
  assert.equal(saved.notice.key,'authoringSaved');
});

test('replies issued to a previous owner never touch the current session',()=>{
  const previous=type(openSession(packageView('rev-1',files)),'main.ts','A');
  const save=saveTicket(previous,'main.ts');
  const validation=validationTicket(previous);
  const catalog=catalogTicket(previous,{kind:'remove',path:'helper.ts'});
  const duplicate=type(openSession(packageView('rev-9',files,'lease-2')),'main.ts','X');
  assert.equal(applySave(duplicate,save,mutation('rev-2',null)),duplicate);
  assert.equal(applyValidation(duplicate,validation,{owner,revision:'rev-1',valid:true,diagnostics:[]}),duplicate);
  assert.equal(applyCatalogMutation(duplicate,catalog,mutation('rev-2',null)),duplicate);
  assert.equal(failCommand(duplicate,save.token,{category:AUTHORING_CONFLICT,message:'stale',context:null}),duplicate);
  assert.equal(applyRefresh(duplicate,packageView('rev-3',files)),duplicate);
  assert.equal(applySave(null,save,mutation('rev-2',null)),null);
});

test('a committed Save whose refresh failed stays committed; a committed catalog edit requires refresh first',()=>{
  const refreshError={category:'Io',message:'read failed',context:null};
  let state=type(type(openSession(packageView('rev-1',files)),'main.ts','A'),'helper.ts','H');
  state=applySave(state,saveTicket(state,'main.ts'),mutation('rev-2',null,refreshError));
  assert.equal(state.revision,'rev-2');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.deepEqual(state.refreshError,refreshError);
  assert.equal(state.notice.key,'authoringSavedRefreshFailed');
  assert.equal(saveBlock(state,'helper.ts'),'refresh');
  assert.equal(saveTicket(state,'helper.ts'),null);
  state=applyRefresh(state,packageView('rev-2',withText('main.ts','A'+MAIN)));
  assert.equal(saveBlock(state,'helper.ts'),null);

  const edit={kind:'add',path:'extra.ts',file_kind:'source',text:''};
  state=applyCatalogMutation(state,catalogTicket(state,edit),mutation('rev-3',null,refreshError));
  assert.equal(state.revision,'rev-3');
  assert.equal(state.refreshRequired,true);
  assert.equal(state.notice.key,'authoringCatalogRefreshFailed');
  assert.equal(saveBlock(state,'helper.ts'),'refresh');
  assert.equal(catalogBlock(state,edit),'refresh');
  state=applyRefresh(state,packageView('rev-3',[...withText('main.ts','A'+MAIN),file('extra.ts','source','')]));
  assert.equal(state.refreshRequired,false);
  assert.equal(state.refreshError,null);
  assert.equal(text(state,'helper.ts'),'H'+HELPER);
  assert.equal(state.drafts.has('extra.ts'),true);
  assert.equal(saveBlock(state,'helper.ts'),null);
});

test('a stale-source refusal keeps drafts until a deliberate refresh reconciles them with the disk',()=>{
  let state=type(type(openSession(packageView('rev-1',files)),'main.ts','A'),'helper.ts','H');
  const ticket=saveTicket(state,'main.ts');
  state=failCommand(beginPending(state,{kind:'save',path:'main.ts'}),ticket.token,{category:AUTHORING_CONFLICT,message:'source changed',context:{path:'main.ts'}});
  assert.equal(state.conflict,true);
  assert.equal(state.notice.key,'authoringConflict');
  assert.equal(text(state,'main.ts'),'A'+MAIN);
  assert.equal(saveBlock(state,'main.ts'),'refresh');
  // The disk now has another main.ts and manifest, and helper.ts is no longer declared.
  const disk=[file('package.json','manifest','{"changed":true}\n'),file('main.ts','source','external\n'),file('schema.json','schema','{}\n'),file('images/logo.png','asset',null)];
  state=applyRefresh(state,packageView('rev-5',disk));
  const main=state.drafts.get('main.ts');
  assert.equal(state.conflict,false);
  assert.equal(state.revision,'rev-5');
  assert.deepEqual([main.text,main.base,main.diskChanged],['A'+MAIN,'external\n',true]);
  assert.equal(text(state,'package.json'),'{"changed":true}\n');
  const helper=state.drafts.get('helper.ts');
  assert.deepEqual([helper.text,helper.missing],['H'+HELPER,true]);
  assert.equal(saveBlock(state,'helper.ts'),'missing');
  assert.deepEqual(state.notice,{key:'authoringDiskChanged',args:[1]});
  state=discardFile(state,'main.ts');
  assert.deepEqual([text(state,'main.ts'),fileDirty(state.drafts.get('main.ts')),state.drafts.get('main.ts').diskChanged],['external\n',false,false]);
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'A'+MAIN);
  state=discardFile(state,'helper.ts');
  assert.equal(state.drafts.has('helper.ts'),false);
});

test('validation describes only its captured revision and a late conflict diagnostic requires refresh',()=>{
  let state=openSession(packageView('rev-1',files));
  const ticket=validationTicket(state);
  state=type(state,'main.ts','A');
  state=applySave(state,saveTicket(state,'main.ts'),mutation('rev-2',packageView('rev-2',withText('main.ts','A'+MAIN))));
  state=applyValidation(beginPending(state,{kind:'validate'}),ticket,{owner,revision:'rev-1',valid:true,diagnostics:[]});
  assert.equal(state.validation.revision,'rev-1');
  assert.equal(validationCurrent(state),false);
  assert.equal(state.notice.key,'authoringEarlierValidated');
  assert.equal(state.pending,null);
  const current=validationTicket(state);
  const conflict={category:AUTHORING_CONFLICT,message:'source changed before capture',context:null};
  state=applyValidation(state,current,{owner,revision:'rev-2',valid:false,diagnostics:[conflict]});
  assert.equal(state.conflict,true);
  assert.equal(saveBlock(state,'helper.ts'),'refresh');
  assert.equal(validationTicket(beginPending(state,{kind:'refresh'})),null);
});

test('catalog edits wait for dirty manifest or target drafts and a rename keeps the clean draft history',()=>{
  const clean=openSession(packageView('rev-1',files));
  assert.equal(catalogBlock(type(clean,'package.json','x'),{kind:'add',path:'x.ts',file_kind:'source',text:''}),'manifestDirty');
  const dirtyHelper=type(clean,'helper.ts','x');
  assert.equal(catalogBlock(dirtyHelper,{kind:'rename',path:'helper.ts',destination:'lib/helper.ts'}),'fileDirty');
  assert.equal(catalogBlock(dirtyHelper,{kind:'remove',path:'main.ts'}),null);
  let state=type(clean,'main.ts','A');
  state=applySave(state,saveTicket(state,'main.ts'),mutation('rev-2',packageView('rev-2',withText('main.ts','A'+MAIN))));
  const edit={kind:'rename',path:'main.ts',destination:'src/main.ts'};
  const renamed=[file('package.json','manifest','{}\n'),file('src/main.ts','source','A'+MAIN),file('helper.ts','source',HELPER),file('schema.json','schema','{}\n'),file('images/logo.png','asset',null)];
  state=applyCatalogMutation(state,catalogTicket(state,edit),mutation('rev-3',packageView('rev-3',renamed)));
  assert.equal(state.drafts.has('main.ts'),false);
  assert.equal(state.selected,'src/main.ts');
  assert.equal(state.drafts.get('src/main.ts').undo.length,1);
  assert.deepEqual(state.order,renamed.map(item=>item.path));
  assert.equal(state.notice.key,'authoringCatalogSaved');
  state=selectRecognition(state,null);
  const add={kind:'add',path:'new.ts',file_kind:'source',text:''};
  state=applyCatalogMutation(state,catalogTicket(state,add),mutation('rev-4',packageView('rev-4',[...renamed,file('new.ts','source','')])));
  assert.equal(state.destination,'file');
  assert.equal(state.selected,'new.ts');
});

test('search is literal, case-insensitive and wraps; diagnostics map to file offsets',()=>{
  const value='Alpha beta\nALPHA gamma\nalpha';
  assert.deepEqual(findMatch(value,'alpha',{start:0,end:0},false),{start:0,end:5});
  assert.deepEqual(findMatch(value,'alpha',{start:0,end:5},false),{start:11,end:16});
  assert.deepEqual(findMatch(value,'alpha',{start:23,end:28},false),{start:0,end:5});
  assert.deepEqual(findMatch(value,'alpha',{start:0,end:5},true),{start:23,end:28});
  assert.deepEqual(findMatch(value,'alpha',{start:23,end:28},true),{start:11,end:16});
  assert.deepEqual(findMatch('a.b axb','.',{start:0,end:0},false),{start:1,end:2});
  assert.equal(findMatch(value,'',{start:0,end:0},false),null);
  assert.deepEqual(matchSummary(value,'alpha',{start:11,end:16}),{count:3,capped:false,current:2});
  assert.deepEqual(matchSummary(value,'delta',{start:0,end:0}),{count:0,capped:false,current:null});
  const location=diagnosticLocation({category:'TypeScript',message:'x',context:{path:'main.ts',line:2,column:3}});
  assert.deepEqual(location,{path:'main.ts',line:2,column:3});
  assert.equal(offsetAt(value,2,3),13);
  assert.equal(offsetAt(value,1,99),10);
  assert.equal(offsetAt(value,9,1),value.length);
  assert.equal(diagnosticLocation({category:'X',message:'x',context:{line:1}}),null);
  assert.equal(recoveryPath({category:'AuthoringRecoveryRequired',message:'x',context:{package_path:'/pkg/b'}},'/pkg/a'),'/pkg/b');
  assert.equal(recoveryPath({category:'AuthoringRecoveryRequired',message:'x',context:null},'/pkg/a'),'/pkg/a');
});

test('real compiler envelopes expose each diagnostic message and source location for editor navigation',()=>{
  const state=openSession(packageView('rev-1',files));
  const result={owner,revision:'rev-1',valid:false,diagnostics:[{category:'TypeScript',message:'TypeScript compilation failed',context:{diagnostics:[
    {category:'Error',code:2391,module:'main.ts',line:7,column:17,message:'Function implementation is missing or not immediately following the declaration.'},
    {category:'Error',code:1138,module:'main.ts',line:7,column:24,message:'Parameter declaration expected.'},
  ]}}]};
  const next=applyValidation(state,validationTicket(state),result);
  assert.deepEqual(next.validation.diagnostics.map(item=>({message:item.message,location:diagnosticLocation(item)})),[
    {message:'Function implementation is missing or not immediately following the declaration.',location:{path:'main.ts',line:7,column:17}},
    {message:'Parameter declaration expected.',location:{path:'main.ts',line:7,column:24}},
  ]);
  assert.deepEqual(next.notice,{key:'authoringInvalid',args:['rev-1',2]});
});

const schema={type:'object',properties:{count:{type:'integer',default:1}}};
function selection(id,revision,path){
  return {workspace_id:id,revision,internal_name:id,display_name:'Tab '+id,package_path:path,profiles_error:null,profiles:[],
    package:{package_id:'pkg',inventory_identity:'inv-'+id,schema_identity:'schema-1',runtime:'quickjs',schema,profiles:{},effective_defaults:null,target:null,target_identity:null}};
}
function workspaceView(id,revision,bound,path='/pkg/edited'){
  return {workspace_id:id,revision,internal_name:id,display_name:'Tab '+id,selection:bound,source_error:null,saved_package:{package_id:'pkg',source:{kind:'directory',path}},recovery:null};
}

test('leaving Edit requires explicit reinspection and invalidates only the Tabs the host moved',()=>{
  const ownerTab={...workspaceFromView(workspaceView('a',1,selection('a',1,'/pkg/edited'))),page:'edit'};
  const exited=applyAuthoringExit(ownerTab,workspaceView('a',2,null),'/pkg/edited');
  assert.deepEqual([exited.revision,exited.bound,exited.page,exited.inspectPath,exited.notice],[2,null,'run','/pkg/edited',{key:'authoringExited'}]);
  const sameRoot=updateBound(workspaceFromView(workspaceView('b',1,selection('b',1,'/pkg/edited'))),bound=>editDraft(bound,{count:'7'}));
  const otherRoot=updateBound(workspaceFromView(workspaceView('c',1,selection('c',1,'/pkg/other'),'/pkg/other')),bound=>editDraft(bound,{count:'9'}));
  const list=[exited,sameRoot,otherRoot];
  const next=applyInvalidatedViews(list,[workspaceView('a',2,null),workspaceView('b',2,null),workspaceView('c',1,selection('c',1,'/pkg/other'),'/pkg/other')]);
  assert.equal(next[0],exited);
  assert.deepEqual([next[1].revision,next[1].bound,next[1].notice],[2,null,{key:'selectionInvalidated'}]);
  assert.equal(next[2],otherRoot);
  assert.equal(applyInvalidatedViews(next,[workspaceView('b',2,null)]),next);
});

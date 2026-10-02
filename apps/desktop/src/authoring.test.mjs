import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AUTHORING_CONFLICT,AUTHORING_NON_IMAGE_BYTES,AUTHORING_SOURCE_BYTES,MATCH_LIMIT,applyCatalogMutation,applyRecognitionMutation,applyRefresh,applySave,applyValidation,beginComposition,beginPending,catalogBlock,catalogTicket,diagnosticLocation,discardFile,editFile,endComposition,failCommand,fileDirty,findMatch,lineColumn,lineCount,matchSummary,offsetAt,openSession,otherNonImageBytes,recoveryPath,redoFile,replaceFile,replacementEdit,revealRange,saveBlock,saveTicket,selectFile,selectRecognition,undoFile,validationCurrent,validationTicket} from './authoring.ts';
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
// Inserts at the draft's caret: new text/selection and the selection before the input.
function type(state,path,inserted,inputType='insertText',before){
  const draft=state.drafts.get(path);
  const at=before??draft.range;
  const text=draft.text.slice(0,at.start)+inserted+draft.text.slice(at.end);
  const caret=at.start+inserted.length;
  return editFile(state,path,{text,start:caret,end:caret},at,{type:inputType,data:inserted,composing:false});
}
const text=(state,path)=>state.drafts.get(path).text;

function replacementTicket(state,path=state.selected){
  return {owner:state.owner.token,path,revision:state.drafts.get(path).revision};
}
function replace(state,query,replacement,all=false,range=state.drafts.get(state.selected).range){
  const ticket=replacementTicket(state);
  const result=replacementEdit(state,ticket,query,replacement,all,range);
  if(result.kind==='edit') return editFile(state,ticket.path,result.next,range,{type:'insertReplacement',data:replacement,composing:false});
  if(result.kind==='select') return revealRange(state,ticket.path,result.range);
  return state;
}

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

test('Replace selects the next full match without editing an unmatched or partial selection and wraps',()=>{
  let state=openSession(packageView('rev-1',withText('main.ts','foo FOO foo')));
  state=replace(state,'foo','bar',false,{start:1,end:2});
  assert.deepEqual(state.drafts.get('main.ts').range,{start:4,end:7});
  assert.equal(text(state,'main.ts'),'foo FOO foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  state=replace(state,'foo','bar');
  assert.equal(text(state,'main.ts'),'foo bar foo');
  state=undoFile(state,'main.ts');
  assert.deepEqual(state.drafts.get('main.ts').range,{start:4,end:7});
  state=replace(state,'foo','bar',false,{start:11,end:11});
  assert.deepEqual(state.drafts.get('main.ts').range,{start:0,end:3});
  assert.equal(text(state,'main.ts'),'foo FOO foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
});

test('selected replacement is a separate per-file Undo action restoring prior text and selection',()=>{
  let state=openSession(packageView('rev-1',withText('main.ts','alpha alpha')));
  state=type(state,'main.ts','draft ');
  state=type(state,'helper.ts','other ');
  state=revealRange(state,'main.ts',{start:6,end:11});
  state=replace(state,'alpha','omega');
  assert.equal(text(state,'main.ts'),'draft omega alpha');
  assert.equal(text(state,'helper.ts'),'other '+HELPER);
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'draft alpha alpha');
  assert.deepEqual(state.drafts.get('main.ts').range,{start:6,end:11});
  assert.equal(fileDirty(state.drafts.get('main.ts')),true);
  assert.equal(text(state,'helper.ts'),'other '+HELPER);
  state=redoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'draft omega alpha');
  state=undoFile(undoFile(state,'main.ts'),'main.ts');
  assert.equal(text(state,'main.ts'),'alpha alpha');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(text(state,'helper.ts'),'other '+HELPER);
});

test('Replace all reaches beyond the display cap, inserts literal metacharacters once and undoes atomically',()=>{
  const original='A '.repeat(MATCH_LIMIT+3);
  const replacement='a$&$1$$\\a';
  let state=openSession(packageView('rev-1',withText('main.ts',original)));
  assert.deepEqual(matchSummary(original,'a',{start:0,end:1}),{count:MATCH_LIMIT,capped:true,current:1});
  state=replace(state,'a',replacement,true);
  assert.equal(text(state,'main.ts'),(replacement+' ').repeat(MATCH_LIMIT+3));
  assert.equal(text(state,'helper.ts'),HELPER);
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),original);
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  state=redoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),(replacement+' ').repeat(MATCH_LIMIT+3));
});

test('literal regex-looking queries and overlapping occurrences keep original non-overlapping replacement semantics',()=>{
  const query='[$.*?+^{}()|\\]';
  const replacement='$&$1$$\\';
  const literal=openSession(packageView('rev-1',withText('main.ts',query+' other '+query)));
  assert.equal(text(replace(literal,query,replacement,true),'main.ts'),replacement+' other '+replacement);
  const overlapping=openSession(packageView('rev-1',withText('main.ts','aaaaa')));
  assert.equal(text(replace(overlapping,'aa','aaa',true),'main.ts'),'aaaaaaa');
});

test('empty replacement deletes selected and remaining matches with one Undo action each',()=>{
  let state=openSession(packageView('rev-1',withText('main.ts','foo foo')));
  state=replace(state,'foo','',false,{start:0,end:3});
  assert.equal(text(state,'main.ts'),' foo');
  state=replace(state,'foo','',true);
  assert.equal(text(state,'main.ts'),' ');
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),' foo');
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'foo foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
});

const noOpReplacements=[
  {scenario:'empty query leaves draft history unchanged',query:'',replacement:'bar',all:true},
  {scenario:'unmatched query leaves draft history unchanged',query:'missing',replacement:'bar',all:true},
  {scenario:'identical selected replacement leaves draft history unchanged',query:'foo',replacement:'foo',all:false},
  {scenario:'identical Replace all leaves draft history unchanged',query:'foo',replacement:'foo',all:true},
];
for(const {scenario,query,replacement,all} of noOpReplacements) test(scenario,()=>{
  let state=openSession(packageView('rev-1',withText('main.ts','foo foo')));
  state=undoFile(type(state,'main.ts','later '),'main.ts');
  const before=state;
  assert.deepEqual(replacementEdit(state,replacementTicket(state),query,replacement,all,{start:0,end:3}),{kind:'noop'});
  state=replace(state,query,replacement,all,{start:0,end:3});
  assert.equal(state,before);
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(state.drafts.get('main.ts').revision,before.drafts.get('main.ts').revision);
  assert.equal(text(redoFile(state,'main.ts'),'main.ts'),'later foo foo');
});

const staleReplacements=[
  {scenario:'a replacement ticket cannot enter a successor owner with equal path, revision and text',
    change:state=>openSession(packageView('rev-1',withText('main.ts','foo'),'lease-next'))},
  {scenario:'a replacement ticket cannot edit a file after selection moves elsewhere',
    change:state=>selectFile(state,'helper.ts',null)},
  {scenario:'a replacement ticket cannot edit an advanced revision even after Undo restores the same text',
    change:state=>undoFile(type(state,'main.ts','later '),'main.ts')},
];
for(const {scenario,change} of staleReplacements) test(scenario,()=>{
  const initial=openSession(packageView('rev-1',withText('main.ts','foo')));
  const ticket=replacementTicket(initial);
  const state=change(initial);
  assert.deepEqual(replacementEdit(state,ticket,'foo','bar',true,{start:0,end:3}),{kind:'refused',reason:'stale'});
  assert.equal(text(state,'main.ts'),'foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(text(state,'helper.ts'),HELPER);
});

const ineligibleReplacements=[
  {scenario:'replacement cannot interrupt composition',change:state=>beginComposition(state,'main.ts',{start:0,end:0})},
  {scenario:'replacement cannot edit during owner exit',change:state=>beginPending(state,{kind:'exit'})},
  {scenario:'replacement cannot edit during duplication',change:state=>beginPending(state,{kind:'duplicate'})},
  {scenario:'replacement cannot edit an undisplayed source in Recognition',change:state=>selectRecognition(state,null)},
  {scenario:'replacement cannot rewrite structured metadata',change:state=>selectFile(state,'schema.json',null)},
  {scenario:'replacement cannot rewrite binary assets',change:state=>selectFile(state,'images/logo.png',null)},
];
for(const {scenario,change} of ineligibleReplacements) test(scenario,()=>{
  const state=change(openSession(packageView('rev-1',withText('main.ts','foo'))));
  assert.deepEqual(replacementEdit(state,replacementTicket(state),'foo','bar',true,{start:0,end:3}),{kind:'refused',reason:'ineligible'});
  assert.equal(text(state,'main.ts'),'foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(text(state,'schema.json'),'{}\n');
  assert.equal(text(state,'images/logo.png'),null);
});

test('replacement refuses a removed source while preserving its recoverable unsaved draft',()=>{
  let state=type(openSession(packageView('rev-1',withText('main.ts','foo'))),'main.ts','draft ');
  state=applyRefresh(state,packageView('rev-2',files.filter(item=>item.path!=='main.ts')));
  assert.equal(state.drafts.get('main.ts').missing,true);
  assert.deepEqual(replacementEdit(state,replacementTicket(state),'foo','bar',true,{start:6,end:9}),{kind:'refused',reason:'ineligible'});
  assert.equal(text(state,'main.ts'),'draft foo');
  assert.equal(text(undoFile(state,'main.ts'),'main.ts'),'foo');
});

test('replacement preflights exact UTF-8 source bytes and refuses overflow without changing history',()=>{
  const state=openSession(packageView('rev-1',[file('main.ts','source','x')]));
  const ticket=replacementTicket(state);
  const bounded='日'.repeat(Math.floor(AUTHORING_SOURCE_BYTES/3))+'a';
  assert.equal(new TextEncoder().encode(bounded).length,AUTHORING_SOURCE_BYTES);
  assert.deepEqual(replacementEdit(state,ticket,'x',bounded,true,{start:0,end:1}),
    {kind:'edit',next:{text:bounded,start:bounded.length,end:bounded.length}});
  assert.deepEqual(replacementEdit(state,ticket,'x',bounded+'a',true,{start:0,end:1}),{kind:'refused',reason:'oversized'});
  assert.equal(replace(state,'x',bounded+'a',true),state);
  assert.equal(text(state,'main.ts'),'x');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(undoFile(state,'main.ts'),state);
});

test('replacement counts surrogate pairs joined across output segments before refusing the whole result',()=>{
  const prefix='a'.repeat(AUTHORING_SOURCE_BYTES-4);
  let state=openSession(packageView('rev-1',[file('main.ts','source',prefix+'😀')]));
  state=type(state,'main.ts','X','insertFromPaste',{start:prefix.length+1,end:prefix.length+1});
  const result=replacementEdit(state,replacementTicket(state),'X','',true,{start:0,end:0});
  assert.deepEqual(result,{kind:'edit',next:{text:prefix+'😀',start:prefix.length+1,end:prefix.length+1}});
  state=replace(state,'X','',true);
  assert.equal(text(state,'main.ts'),prefix+'😀');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  assert.equal(text(undoFile(state,'main.ts'),'main.ts'),prefix+'\ud83dX\ude00');
});

test('aggregate non-image preflight includes current metadata and binary JSON assets but not declared images',()=>{
  const manifest=JSON.stringify({assets:{pixels:{path:'pixels.bin',format:'raw-rgba8'},logo:{path:'looks-like-json.json',format:'png'},
    data:{path:'pretends-image.png',format:'json'}}});
  const inventory=[
    file('package.json','manifest',manifest),file('main.ts','source','x'),file('helper.ts','source','abc'),file('schema.json','schema','日'),
    file('profile.json','profile','{"x":1}'),file('main.map','source_map','😀'),
    {...file('pixels.bin','asset',null),bytes:32*1024*1024},{...file('looks-like-json.json','asset',null),bytes:1024*1024},
    {...file('pretends-image.png','asset',null),bytes:11},
  ];
  let state=type(openSession(packageView('rev-1',inventory)),'helper.ts','日');
  const expected=new TextEncoder().encode(manifest).length+6+3+7+4+11;
  assert.equal(otherNonImageBytes(state,'main.ts'),expected);
  const bounded='a'.repeat(AUTHORING_NON_IMAGE_BYTES-expected);
  state=replace(state,'x',bounded,true);
  assert.equal(text(state,'main.ts'),bounded);
  assert.equal(text(state,'helper.ts'),'日abc');
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'x');
  assert.deepEqual(replacementEdit(state,replacementTicket(state),'x',bounded+'a',true,{start:0,end:1}),{kind:'refused',reason:'oversized'});
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  // Current text changes count immediately, but an unsaved manifest cannot exempt a disk JSON asset as an image.
  const editedManifest=manifest.replace('"json"','"png"');
  state=replaceFile(state,'package.json',editedManifest);
  assert.equal(otherNonImageBytes(state,'main.ts'),new TextEncoder().encode(editedManifest).length+6+3+7+4+11);
  state=applyRefresh(state,packageView('rev-2',inventory.filter(item=>item.path!=='helper.ts')));
  assert.equal(state.drafts.get('helper.ts').missing,true);
  assert.equal(otherNonImageBytes(state,'main.ts'),new TextEncoder().encode(editedManifest).length+3+7+4+11);
});

test('UTF-16 search and diagnostic positions support CRLF, lone CR and LF without normalizing source',()=>{
  const original='一😀\r\nA😀\rB\nC';
  let state=openSession(packageView('rev-1',withText('main.ts',original)));
  assert.equal(lineCount(original),4);
  assert.equal(offsetAt(original,1,99),3);
  assert.equal(offsetAt(original,2,1),5);
  assert.equal(offsetAt(original,2,99),8);
  assert.equal(offsetAt(original,3,1),9);
  assert.equal(offsetAt(original,4,1),11);
  assert.equal(offsetAt(original,5,1),original.length);
  assert.deepEqual(lineColumn(original,6),{line:2,column:2});
  assert.deepEqual(lineColumn(original,7),{line:2,column:3});
  assert.deepEqual(lineColumn(original,9),{line:3,column:1});
  assert.deepEqual(findMatch(original,'😀',{start:1,end:3},false),{start:6,end:8});
  state=revealRange(state,'main.ts',{start:6,end:8});
  assert.equal(text(state,'main.ts'),original);
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  state=replace(state,'😀','終');
  assert.equal(text(state,'main.ts'),'一😀\r\nA終\rB\nC');
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),original);
  assert.deepEqual(state.drafts.get('main.ts').range,{start:6,end:8});
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
});

test('Save of replacement A preserves replacement B and its Undo baseline while the save is pending',()=>{
  let state=openSession(packageView('rev-1',withText('main.ts','foo foo')));
  state=replace(state,'foo','A',false,{start:0,end:3});
  const ticket=saveTicket(state,'main.ts');
  state=beginPending(state,{kind:'save',path:'main.ts'});
  state=replace(state,'foo','B',true);
  state=applySave(state,ticket,mutation('rev-2',packageView('rev-2',withText('main.ts','A foo'))));
  assert.equal(text(state,'main.ts'),'A B');
  assert.equal(state.drafts.get('main.ts').base,'A foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),true);
  assert.equal(saveTicket(state,'main.ts').text,'A B');
  state=undoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'A foo');
  assert.equal(fileDirty(state.drafts.get('main.ts')),false);
  state=redoFile(state,'main.ts');
  assert.equal(text(state,'main.ts'),'A B');
  assert.equal(fileDirty(state.drafts.get('main.ts')),true);
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
